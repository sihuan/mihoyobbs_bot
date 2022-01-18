from config import TOKEN,CHAT_ID

from time import sleep
from typing import NamedTuple
import telegram
import redis
import requests
import logging
import random
import re

logging.basicConfig(level=logging.WARNING,
                    format='%(asctime)s - %(name)s - %(levelname)s - %(message)s')
post_url_base = "https://bbs.mihoyo.com/ys/article/"
api_url_base = "https://bbs-api.mihoyo.com/post/wapi/getNewsList?gids=2&page_size=5&type="
r = redis.StrictRedis(host='localhost', port=6379, db=3)

class Post(NamedTuple):
    img_url: str
    topics: list[str]
    content: str
    subject: str
    post_id: str

def set_post(post: Post) -> None:
    r.hmset(post.post_id, {
        'img_url': post.img_url,
        'topics': ','.join(post.topics),
        'content': post.content,
        'subject': post.subject
    })

def get_post(post_id: str) -> Post:
    post = r.hgetall(post_id)
    return Post(post['img_url'],
                post['topics'].split(','),
                post['content'],
                post['subject'],
                post_id)

def get_posts() -> list[Post]:
    posts = []
    just_a_map = {
        '公告': '1',
        '活动': '2',
        '资讯': '3',
    }
    for k, v in just_a_map.items():
        resp = requests.get(api_url_base + v)
        resp = resp.json()
        for post in resp['data']['list']:
            if r.exists(post['post']['post_id']):
                break
            logging.debug(f'{k} {post["post"]["subject"]}')
            posts.append(Post(post['image_list'][0]['url'],
                              [k] + [topics['name']
                                     for topics in post['topics']],
                              post['post']['content'],
                              post['post']['subject'],
                              post['post']['post_id']))
    return posts

def fuck_telegram_markdown(text: str) -> str:
    parse = re.sub(r"([_*\[\]()~`>\#\+\-=|\.!])", r"\\\1", text)
    reparse = re.sub(r"\\\\([_*\[\]()~`>\#\+\-=|\.!])", r"\1", parse)
    return reparse

def send_post(post: Post, bot: telegram.Bot) -> None:
    tags = ' '.join([f'\#{tag}' for tag in post.topics])
    content = fuck_telegram_markdown(post.content)
    subject = fuck_telegram_markdown(post.subject)
    url = post.img_url

    attempts = 0
    success = False
    while attempts < 3 and not success:
        try:
            bot.send_photo(photo=url,
                       chat_id=CHAT_ID,
                       parse_mode=telegram.constants.PARSEMODE_MARKDOWN_V2,
                       caption=f'{tags}\n\n*{subject}*\n  {content}……\n[Read more\.\.\.]({post_url_base + post.post_id})'
                       )
            success = True
        except:
            logging.warning(f'Retry to send post {post.post_id}, times {attempts}')
            if attempts == 0:
                url += '?'
            url += str(random.randint(100,999))
            attempts += 1
            if attempts == 3:
                break
    if not success:
        logging.error(f'Failed to send post {post.subject} {post.post_id} {post.img_url}')

def main():
    bot = telegram.Bot(token=TOKEN)
    posts = get_posts()
    posts.sort(key=lambda x: x.post_id)
    for post in posts:
        send_post(post, bot)
        sleep(1)
        set_post(post)

if __name__ == '__main__':
    main()
