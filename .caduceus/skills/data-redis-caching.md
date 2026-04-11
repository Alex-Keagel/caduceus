---
name: data-redis-caching
version: "1.0"
description: Redis caching patterns — cache-aside, TTL strategy, distributed locks, rate limiting, and pub/sub
categories: [data, caching, backend]
triggers: ["redis cache aside", "redis ttl strategy", "redis distributed lock", "redis rate limiter", "redis pubsub pattern"]
tools: [read_file, edit_file, run_tests, shell]
---

# Redis Caching Skill

## Setup
```bash
docker run -d --name redis -p 6379:6379 redis:7-alpine
pip install redis[hiredis]   # Python — hiredis gives ~10x faster parsing
npm install ioredis           # Node.js
```

## Cache-Aside Pattern (Python)
```python
import redis.asyncio as aioredis
import json

r = aioredis.from_url("redis://localhost:6379", decode_responses=True)

async def get_user(user_id: str) -> dict | None:
    key = f"user:{user_id}"
    cached = await r.get(key)
    if cached is not None:
        return json.loads(cached)

    user = await db.fetch_user(user_id)
    if user:
        await r.setex(key, 300, json.dumps(user))   # 5-minute TTL
    return user

async def invalidate_user(user_id: str) -> None:
    await r.delete(f"user:{user_id}")
```

## Key Naming Convention
Format: `<namespace>:<entity>:<id>[:<qualifier>]`

| Example | Meaning |
|---------|---------|
| `user:123` | Cached user object |
| `session:abc123` | Session data |
| `rate_limit:ip:10.0.0.1` | Rate limit counter |
| `lock:order:456` | Distributed lock for order processing |

## TTL Strategy
| Data type | TTL recommendation |
|-----------|-------------------|
| Session data | 30–300 s |
| User profiles | 1–24 h |
| Reference / config data | 24 h; invalidate on write |
| Rate limit windows | Match window duration exactly |

## Distributed Lock
```python
async def with_exclusive_lock(resource_id: str, timeout_s: int = 10):
    lock = r.lock(f"lock:{resource_id}", timeout=timeout_s)
    async with lock:
        yield   # critical section; only one instance enters at a time
```

## Sliding Window Rate Limiter
```python
import time

async def is_rate_limited(identifier: str, limit: int, window_s: int) -> bool:
    key = f"rate:{identifier}"
    now = time.time()
    async with r.pipeline(transaction=True) as pipe:
        pipe.zremrangebyscore(key, 0, now - window_s)
        pipe.zcard(key)
        pipe.zadd(key, {str(now): now})
        pipe.expire(key, window_s)
        _, count, *_ = await pipe.execute()
    return count >= limit
```

## Pub/Sub
```python
async def publish_event(channel: str, payload: dict) -> None:
    await r.publish(channel, json.dumps(payload))

async def subscribe_to_events(channel: str):
    async with r.pubsub() as pubsub:
        await pubsub.subscribe(channel)
        async for message in pubsub.listen():
            if message["type"] == "message":
                yield json.loads(message["data"])
```

## Operational Rules
- Set `maxmemory-policy allkeys-lru` on cache-only Redis instances
- Use connection pooling — `from_url` creates a pool automatically in `redis-py`
- Never create a new connection per request
- Define and document invalidation strategy before caching any mutable data
