---
name: messaging-kafka
version: "1.0"
description: Kafka producer/consumer patterns — topics, partitions, consumer groups, offsets, and schema registry
categories: [messaging, streaming, backend]
triggers: ["kafka producer consumer", "kafka topic partition", "kafka consumer group", "apache kafka python", "kafka schema registry"]
tools: [read_file, edit_file, run_tests, shell]
---

# Kafka Producer/Consumer Skill

## Local Setup (KRaft mode, no ZooKeeper)
```bash
docker run -d --name kafka -p 9092:9092 \
  -e KAFKA_CFG_NODE_ID=0 \
  -e KAFKA_CFG_PROCESS_ROLES=controller,broker \
  -e KAFKA_CFG_LISTENERS=PLAINTEXT://:9092,CONTROLLER://:9093 \
  -e KAFKA_CFG_ADVERTISED_LISTENERS=PLAINTEXT://localhost:9092 \
  bitnami/kafka:latest
```

## Python Client
```bash
pip install confluent-kafka
```

## Producer Pattern
```python
from confluent_kafka import Producer

producer = Producer({
    "bootstrap.servers": "localhost:9092",
    "acks": "all",                  # wait for all in-sync replicas
    "enable.idempotence": True,     # exactly-once producer semantics
    "compression.type": "snappy",
})

def delivery_report(err, msg):
    if err:
        print(f"Delivery failed for {msg.key()}: {err}")

producer.produce(
    "orders",
    key=str(order.id),
    value=order.model_dump_json(),
    on_delivery=delivery_report,
)
producer.flush()     # block until all messages are confirmed delivered
```

## Consumer Pattern (at-least-once with manual commit)
```python
from confluent_kafka import Consumer, KafkaException

consumer = Consumer({
    "bootstrap.servers": "localhost:9092",
    "group.id": "order-processor",
    "auto.offset.reset": "earliest",
    "enable.auto.commit": False,    # manual commit for reliability
})
consumer.subscribe(["orders"])

while True:
    msg = consumer.poll(timeout=1.0)
    if msg is None:
        continue
    if msg.error():
        raise KafkaException(msg.error())
    process(msg.value())
    consumer.commit(asynchronous=False)   # commit only after processing
```

## Key Design Decisions
- **Partition key**: use entity ID (e.g., `order_id`) to guarantee ordering per entity
- **Consumer groups**: one partition is assigned to at most one consumer — scale by adding partitions
- **Retention**: set `retention.ms` and `retention.bytes` per topic; default 7 days is usually safe
- **Exactly-once**: enable `enable.idempotence=True` on producer; use transactional API for critical paths

## Topic Management
```bash
kafka-topics.sh --create --topic orders \
  --partitions 12 --replication-factor 3 \
  --bootstrap-server localhost:9092 \
  --config retention.ms=604800000   # 7 days
```

## Schema Registry
- Register Avro or Protobuf schemas in Confluent Schema Registry for schema evolution control
- Use `BACKWARD` compatibility mode by default; breaking changes require a new topic or `FULL_TRANSITIVE`
- Validate schema registration in CI before deploying producers

## Monitoring
- Track consumer lag per group/partition; alert when lag exceeds processing SLA (e.g., > 10k messages)
- Use `kafka-consumer-groups.sh --describe --group my-group` for lag inspection
