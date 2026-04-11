---
name: messaging-rabbitmq
version: "1.0"
description: RabbitMQ messaging patterns — exchanges, queues, routing keys, dead letter handling, and reliability
categories: [messaging, backend, integration]
triggers: ["rabbitmq exchange queue", "amqp publisher consumer", "rabbitmq dead letter", "rabbitmq topic routing", "rabbitmq reliability"]
tools: [read_file, edit_file, run_tests, shell]
---

# RabbitMQ Messaging Skill

## Local Setup
```bash
docker run -d --name rabbitmq -p 5672:5672 -p 15672:15672 rabbitmq:3-management
# Management UI: http://localhost:15672  credentials: guest / guest
```

## Python Client
```bash
pip install pika
```

## Publisher Pattern
```python
import pika, json

conn = pika.BlockingConnection(pika.URLParameters("amqp://guest:guest@localhost/"))
channel = conn.channel()

channel.exchange_declare("orders", exchange_type="topic", durable=True)
channel.basic_publish(
    exchange="orders",
    routing_key="orders.created",
    body=json.dumps({"order_id": 123, "customer_id": "C1"}),
    properties=pika.BasicProperties(
        delivery_mode=2,                    # persistent — survives broker restart
        content_type="application/json",
    ),
)
conn.close()
```

## Consumer with Manual Acks and Dead Letter Exchange
```python
channel.queue_declare("order-processor", durable=True, arguments={
    "x-dead-letter-exchange": "orders.dlx",   # route failed messages here
    "x-message-ttl": 60_000,                  # expire after 60s (then DLX)
})
channel.queue_bind("order-processor", "orders", routing_key="orders.#")

def callback(ch, method, props, body):
    try:
        process(json.loads(body))
        ch.basic_ack(method.delivery_tag)     # success — remove from queue
    except Exception as e:
        print(f"Processing failed: {e}")
        ch.basic_nack(method.delivery_tag, requeue=False)  # route to DLX

channel.basic_qos(prefetch_count=5)           # process at most 5 at a time
channel.basic_consume("order-processor", callback)
channel.start_consuming()
```

## Exchange Types
| Type | Routing | Use Case |
|------|---------|----------|
| `direct` | Exact routing key match | Task queues, work distribution |
| `topic` | Wildcard routing (`*`, `#`) | Event routing by category/verb |
| `fanout` | Broadcasts to all bound queues | Notifications, cache invalidation |
| `headers` | Message header attributes | Complex conditional routing |

## Dead Letter Exchange Setup
```python
channel.exchange_declare("orders.dlx", exchange_type="fanout", durable=True)
channel.queue_declare("orders.dead", durable=True)
channel.queue_bind("orders.dead", "orders.dlx")
```
Alert when the dead letter queue grows; inspect and replay messages manually.

## Reliability Checklist
- `durable=True` on all exchanges and queues
- `delivery_mode=2` (persistent) on every published message
- Manual `basic_ack` only after successful processing — never auto-ack
- Publisher confirms: `channel.confirm_delivery()`; check `basic_publish` return value
- Implement retry with exponential backoff before routing to DLX after max attempts
