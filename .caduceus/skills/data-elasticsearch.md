---
name: data-elasticsearch
version: "1.0"
description: Elasticsearch indexing, mapping design, full-text search with filters, aggregations, and index management
categories: [data, search, backend]
triggers: ["elasticsearch index mapping", "elasticsearch full text search", "elasticsearch filter query", "elasticsearch aggregation", "elastic index alias reindex"]
tools: [read_file, edit_file, shell, run_tests]
---

# Elasticsearch Indexing and Search Skill

## Local Setup
```bash
docker run -d --name elasticsearch -p 9200:9200 \
  -e "discovery.type=single-node" \
  -e "xpack.security.enabled=false" \
  elasticsearch:8.13.0
pip install elasticsearch[async]
```

## Index Mapping Design
```python
from elasticsearch import AsyncElasticsearch

es = AsyncElasticsearch(["http://localhost:9200"])

mapping = {
    "settings": {
        "analysis": {
            "analyzer": {
                "english_analyzer": {"type": "english"}
            }
        },
        "number_of_shards": 2,
        "number_of_replicas": 1,
    },
    "mappings": {
        "properties": {
            "title":       {"type": "text",    "analyzer": "english_analyzer"},
            "description": {"type": "text",    "analyzer": "english_analyzer"},
            "tags":        {"type": "keyword"},
            "price":       {"type": "float"},
            "created_at":  {"type": "date"},
            "category_id": {"type": "keyword"},
        }
    }
}
await es.indices.create(index="products", body=mapping, ignore=400)
```

## Indexing Documents
```python
# Single document
await es.index(index="products", id=product.id, document=product.model_dump())

# Bulk indexing (much faster for large datasets)
from elasticsearch.helpers import async_bulk
actions = [
    {"_index": "products", "_id": p.id, "_source": p.model_dump()}
    for p in products
]
await async_bulk(es, actions, chunk_size=500)
```

## Full-Text + Filter Search
```python
result = await es.search(index="products", body={
    "query": {
        "bool": {
            "must": [
                {"multi_match": {
                    "query": "wireless headphones",
                    "fields": ["title^2", "description"],  # title scores 2x
                    "type": "best_fields",
                }}
            ],
            "filter": [
                {"term": {"tags": "electronics"}},
                {"range": {"price": {"gte": 20, "lte": 200}}},
            ]
        }
    },
    "sort": [{"_score": "desc"}, {"created_at": "desc"}],
    "from": 0, "size": 20,
    "highlight": {"fields": {"title": {}, "description": {}}},
})
hits = result["hits"]["hits"]
```

## Aggregations
```python
agg = await es.search(index="products", body={
    "size": 0,   # don't return documents — only aggregation results
    "aggs": {
        "by_category": {"terms": {"field": "category_id", "size": 20}},
        "price_stats": {"stats": {"field": "price"}},
        "price_histogram": {
            "histogram": {"field": "price", "interval": 50}
        },
    }
})
```

## Zero-Downtime Reindex (Index Aliases)
```bash
# Create new index with updated mapping
PUT /products_v2 { ...new mapping... }

# Reindex data
POST /_reindex { "source": {"index": "products_v1"}, "dest": {"index": "products_v2"} }

# Atomically switch alias
POST /_aliases {
  "actions": [
    { "remove": {"index": "products_v1", "alias": "products"} },
    { "add":    {"index": "products_v2", "alias": "products"} }
  ]
}
```
Application always reads/writes via alias `products` — no code change needed on reindex.
