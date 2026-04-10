# Qdrant behavioral spec for embedded/local semantic code search

Scope: behavioral specification for using Qdrant as the local vector store behind Caduceus semantic code search, derived from the Qdrant source tree at `~/caduceus-reference/qdrant`.

Primary source areas:
- `README.md`
- `lib/edge/**` (embedded/local Rust API)
- `lib/api/src/rest/**`, `lib/api/src/grpc/proto/**`
- `lib/segment/src/types.rs`
- `lib/collection/src/operations/types.rs`
- `src/actix/api/**`
- `openapi/**`

---

## 1. What Qdrant is in this repo

Qdrant has **two distinct usage modes** in this repository:

1. **Server mode** (main `qdrant` binary)
   - HTTP/REST via Actix (`src/actix/api/**`)
   - gRPC via protobuf definitions (`lib/api/src/grpc/proto/**`)
   - Best when Caduceus wants a separate service process.

2. **Embedded/local mode** via **Qdrant Edge**
   - Main type: `EdgeShard` in `lib/edge/src/lib.rs`
   - Published as the separate `qdrant-edge` crate (`lib/edge/README.md`, `lib/edge/publish/**`)
   - In-process, synchronous, no REST/gRPC server required
   - Best fit for “native local vector DB inside a Rust app”

**Important conclusion:** if Caduceus wants to embed Qdrant natively inside Rust, the code in this repo points to **`qdrant-edge` / `EdgeShard`**, not the server crate, as the intended local API.

---

## 2. Core data model

### Collection / shard
Server mode exposes **collections**. Embedded mode exposes an **`EdgeShard`**, which is effectively the local single-shard storage/search engine.

### Point
A point is:
- an ID (`u64` or UUID)
- one or more vectors
- optional payload (arbitrary JSON object)

Relevant types:
- REST point shape: `lib/api/src/rest/schema.rs:1386`
- gRPC point shape: `lib/api/src/grpc/proto/points.proto`
- embedded helper: `lib/edge/src/types/point.rs`

### Vectors
Supported vector styles:
- **dense vectors**
- **sparse vectors**
- **multi-dense vectors**
- **named vectors** (multiple vector spaces per point)

Relevant types:
- `lib/edge/src/types/vector.rs`
- `lib/api/src/grpc/proto/points.proto`
- `lib/segment/src/types.rs`

### Payload
Payload is arbitrary JSON metadata attached to points. Qdrant supports indexing/filtering by payload fields.

Payload schema/index types (`lib/segment/src/types.rs:2120`):
- `keyword`
- `integer`
- `float`
- `geo`
- `text`
- `bool`
- `datetime`
- `uuid`

For code search, payload should carry code metadata such as file path, repo, language, symbol kind, line ranges, AST kind, and content hashes.

---

## 3. Embedded/local Rust API (`qdrant-edge`)

### Main type
`EdgeShard` (`lib/edge/src/lib.rs:42`)

Key methods:
- `EdgeShard::new(path, config)`
- `EdgeShard::load(path, config)`
- `update(...)`
- `search(...)` (**deprecated in edge source; prefer `query(...)`**)
- `query(...)`
- `retrieve(...)`
- `scroll(...)`
- `count(...)`
- `facet(...)`
- `optimize()`
- `flush()`
- `snapshot_manifest()`
- `recover_partial_snapshot(...)`
- `unpack_snapshot(...)`

### Re-exported public API surface
`lib/edge/src/reexports.rs` re-exports the main usable types, including:
- `EdgeShard`, `EdgeConfig`, `EdgeVectorParams`
- `PointStruct`, `PointOperations`, `PointInsertOperations`, `UpdateOperation`
- `SearchRequest`, `QueryRequest`, `ScrollRequest`, `CountRequest`
- `Filter`, `Condition`, `FieldCondition`, `Match`, `Range`
- `CreateIndex`, `FieldIndexOperations`
- `SearchParams`, `HnswIndexConfig`, `QuantizationConfig`

### Minimal embedded usage pattern
Based on `lib/edge/publish/examples/src/lib.rs` and `demo.rs`:

```rust
use std::collections::HashMap;
use std::path::Path;
use qdrant_edge::{
    DEFAULT_VECTOR_NAME, Distance, EdgeConfig, EdgeShard, EdgeVectorParams,
    PointInsertOperations, PointOperations, PointStruct, UpdateOperation,
    SearchRequest, QueryEnum, NamedQuery, WithPayloadInterface, WithVector,
};

let config = EdgeConfig {
    on_disk_payload: false,
    vectors: HashMap::from([(
        DEFAULT_VECTOR_NAME.to_string(),
        EdgeVectorParams {
            size: 768,
            distance: Distance::Cosine,
            on_disk: Some(false),
            multivector_config: None,
            datatype: None,
            quantization_config: None,
            hnsw_config: None,
        },
    )]),
    sparse_vectors: HashMap::new(),
    hnsw_config: Default::default(),
    quantization_config: None,
    optimizers: Default::default(),
};

let shard = EdgeShard::load(Path::new("./qdrant-db"), Some(config))?;

shard.update(UpdateOperation::PointOperation(
    PointOperations::UpsertPoints(
        PointInsertOperations::PointsList(vec![
            PointStruct::new(
                1u64,
                vec![/* embedding */],
                serde_json::json!({
                    "repo": "my-repo",
                    "file_path": "src/lib.rs",
                    "language": "rust",
                    "symbol_kind": "function",
                    "name": "parse_query",
                    "start_line": 10,
                    "end_line": 42,
                    "code": "fn parse_query(...) { ... }"
                })
            ).into()
        ])
    )
))?;
```

### Search in embedded mode
Simple nearest-neighbor search:

```rust
let hits = shard.search(SearchRequest {
    query: QueryEnum::Nearest(NamedQuery {
        query: vec![/* query embedding */].into(),
        using: None,
    }),
    filter: None,
    params: None,
    limit: 20,
    offset: 0,
    with_payload: Some(WithPayloadInterface::Bool(true)),
    with_vector: Some(WithVector::Bool(false)),
    score_threshold: None,
})?;
```

### Preferred query API
`EdgeShard::search()` is marked deprecated in `lib/edge/src/search.rs`; `EdgeShard::query()` is the more general API (`lib/edge/src/query.rs`).

Use `query()` if Caduceus wants:
- prefetch stages
- fusion / hybrid retrieval
- MMR reranking
- formula rescoring
- random/order-by sampling
- unified interface shared with server-side `query` semantics

---

## 4. Server REST API relevant to code search

### Collection lifecycle
From `src/actix/api/collections_api.rs` and `openapi/openapi-collections.ytt.yaml`:
- `GET /collections`
- `GET /collections/{collection_name}`
- `GET /collections/{collection_name}/exists`
- `PUT /collections/{collection_name}`
- `PATCH /collections/{collection_name}`
- `DELETE /collections/{collection_name}`

### Point CRUD
From `src/actix/api/retrieve_api.rs`, `src/actix/api/update_api.rs`, `openapi/openapi-points.ytt.yaml`:
- `GET /collections/{collection_name}/points/{id}`
- `POST /collections/{collection_name}/points` (batch get by IDs)
- `PUT /collections/{collection_name}/points?wait=true` (upsert)
- `POST /collections/{collection_name}/points/delete`
- `PUT /collections/{collection_name}/points/vectors`
- `POST /collections/{collection_name}/points/vectors/delete`
- `POST /collections/{collection_name}/points/payload`
- `PUT /collections/{collection_name}/points/payload`
- `POST /collections/{collection_name}/points/payload/delete`
- `POST /collections/{collection_name}/points/payload/clear`
- `POST /collections/{collection_name}/points/batch`

### Search / query / browse
From `src/actix/api/search_api.rs`, `src/actix/api/retrieve_api.rs`, `openapi/openapi-main.ytt.yaml`:
- `POST /collections/{collection_name}/points/search` (**deprecated**)
- `POST /collections/{collection_name}/points/search/batch` (**deprecated**)
- `POST /collections/{collection_name}/points/search/groups` (**deprecated**)
- `POST /collections/{collection_name}/points/query` (**preferred universal search API**)
- `POST /collections/{collection_name}/points/query/batch`
- `POST /collections/{collection_name}/points/query/groups`
- `POST /collections/{collection_name}/points/scroll`
- `POST /collections/{collection_name}/points/count`
- `POST /collections/{collection_name}/facet`

### Payload index management
From `openapi/openapi-collections.ytt.yaml` and `src/actix/api/update_api.rs`:
- `PUT /collections/{collection_name}/index`
- `DELETE /collections/{collection_name}/index/{field_name}`

For code search this is critical: create indexes on payload fields that are frequently filtered.

---

## 5. gRPC API relevant to code search

Primary proto files:
- `lib/api/src/grpc/proto/collections.proto`
- `lib/api/src/grpc/proto/points.proto`
- `lib/api/src/grpc/proto/qdrant_common.proto`
- `lib/api/src/grpc/proto/points_service.proto`

Key gRPC messages/RPCs include:
- `CreateCollection`
- `UpsertPoints`
- `GetPoints`
- `DeletePoints`
- `UpdatePointVectors`
- `SetPayloadPoints`
- `SearchPoints`
- `ScrollPoints`
- `CountPoints`
- `QueryPoints`
- `CreateFieldIndexCollection`

### Important message shapes

#### `VectorParams` (`collections.proto`)
```proto
uint64 size;
Distance distance;               // Cosine | Euclid | Dot | Manhattan
optional HnswConfigDiff hnsw_config;
optional QuantizationConfig quantization_config;
optional bool on_disk;
optional Datatype datatype;      // Float32 | Uint8 | Float16
optional MultiVectorConfig multivector_config;
```

#### `PointStruct` (`points.proto`)
```proto
PointId id;                      // uint64 or uuid
map<string, Value> payload;
optional Vectors vectors;
```

#### `SearchPoints` (`points.proto`)
Contains:
- `collection_name`
- query vector
- `filter`
- `limit`
- `offset`
- optional `vector_name`
- `SearchParams`
- `score_threshold`

#### `Filter` (`qdrant_common.proto`)
```proto
repeated Condition should;
repeated Condition must;
repeated Condition must_not;
optional MinShould min_should;
```

---

## 6. Request/response behavior you need to implement against

### Create a collection (REST)
Typical shape:

```json
PUT /collections/code_search
{
  "vectors": {
    "size": 768,
    "distance": "Cosine"
  }
}
```

For named vectors:

```json
{
  "vectors": {
    "text": { "size": 768, "distance": "Cosine" },
    "symbol": { "size": 384, "distance": "Dot" }
  }
}
```

### Insert / upsert points
Based on `README.md`, `rest/schema.rs`, and OpenAPI:

```json
PUT /collections/code_search/points?wait=true
{
  "points": [
    {
      "id": 1,
      "vector": [0.1, 0.2, 0.3],
      "payload": {
        "repo": "caduceus",
        "file_path": "src/index.rs",
        "language": "rust",
        "symbol_kind": "function",
        "symbol_name": "embed_chunk",
        "start_line": 120,
        "end_line": 178,
        "hash": "...",
        "code": "fn embed_chunk(...) { ... }"
      }
    }
  ]
}
```

### Search (legacy endpoint)
```json
POST /collections/code_search/points/search
{
  "vector": [0.1, 0.2, 0.3],
  "filter": {
    "must": [
      { "key": "language", "match": { "keyword": "rust" } }
    ]
  },
  "limit": 20,
  "with_payload": true,
  "with_vector": false
}
```

### Query (preferred endpoint)
Use this for future-proof integrations:

```json
POST /collections/code_search/points/query
{
  "query": [0.1, 0.2, 0.3],
  "using": "text",
  "filter": {
    "must": [
      { "key": "repo", "match": { "keyword": "caduceus" } },
      { "key": "language", "match": { "keyword": "rust" } }
    ]
  },
  "limit": 20,
  "with_payload": true,
  "with_vector": false
}
```

### Scroll
Use scroll for deterministic pagination / metadata scans, not semantic ranking:

```json
POST /collections/code_search/points/scroll
{
  "offset": 100,
  "limit": 100,
  "filter": {
    "must": [
      { "key": "file_path", "match": { "keyword": "src/lib.rs" } }
    ]
  },
  "with_payload": true,
  "with_vector": false
}
```

Response includes `next_page_offset` (`lib/collection/src/operations/types.rs:525`).

### Count
```json
POST /collections/code_search/points/count
{
  "filter": {
    "must": [
      { "key": "repo", "match": { "keyword": "caduceus" } }
    ]
  },
  "exact": true
}
```

---

## 7. Vector configuration: dimensions, metrics, datatypes, quantization

### Dimensions
Dense vector dimension is explicit: `EdgeVectorParams.size: usize` / `VectorParams.size`.

Qdrant does **not** impose a small fixed dimension set. It stores whatever dimension you configure per vector name. All inserted vectors for that vector name must match the configured size.

### Distance metrics
From `lib/segment/src/types.rs:308` and `collections.proto:137`:
- `Cosine`
- `Euclid`
- `Dot`
- `Manhattan`

**Recommendation for code search:** use **Cosine** unless your embedding model explicitly expects dot-product scoring.

### Datatypes
From `lib/segment/src/types.rs:1572` and `collections.proto`:
- `float32`
- `float16`
- `uint8`

Practical effect:
- `float32`: safest default
- `float16`: useful memory reduction with usually acceptable recall tradeoff
- `uint8`: aggressive compact storage when upstream embedding pipeline supports/accepts quantized vectors

### Multi-vector support
Supported via `MultiVectorConfig` (`collections.proto`, `types.rs`). This can represent multiple vectors per point under one logical vector field.

For Caduceus, named vectors are usually simpler than multi-vector unless you specifically need per-point token/subchunk vector sets.

### Quantization
From `lib/segment/src/types.rs:769+`:
- **Scalar quantization** (`int8`, optional `quantile`, optional `always_ram`)
- **Product quantization** (`x4`, `x8`, `x16`, `x32`, `x64`, optional `always_ram`)
- **Binary quantization** (encodings: `one_bit`, `two_bits`, `one_and_half_bits`; optional `query_encoding`)

Quantization search parameters (`lib/segment/src/types.rs:508`):
- `ignore`
- `rescore`
- `oversampling`

Behavioral trade-off:
- quantization cuts RAM heavily
- rescoring with original vectors restores quality at extra cost
- if vectors live on disk, rescoring may increase IO latency

**Embedded-mode caveat:** `QuantizationConfig::for_appendable_segment()` currently only enables configs that support appendable segments; in the current source, `supports_appendable()` returns true only for **binary quantization**. In practice, treat appendable-edge quantization support as more constrained than full server mode.

---

## 8. Filtering semantics for code metadata

### Boolean structure
From `qdrant_common.proto` and `lib/segment/src/types.rs`:
- `must` = AND
- `should` = OR
- `must_not` = NOT
- `min_should` = at least N `should` conditions

### Condition types
Supported filter conditions:
- field match/range/text/geo/datetime/value-count
- `is_empty`
- `is_null`
- `has_id`
- `has_vector`
- nested filters
- nested recursive filters

### Match operators
Relevant to code search:
- exact keyword match
- integer match
- boolean match
- full-text `text`
- phrase match
- any-word `text_any`
- multi-value inclusion/exclusion

### Good code-search payload schema
Recommended payload fields:

```json
{
  "repo": "caduceus",
  "workspace": "/abs/path/or/workspace-id",
  "file_path": "src/semantic/index.rs",
  "language": "rust",
  "symbol_kind": "function",
  "symbol_name": "collect_chunks",
  "container": "SemanticIndexer",
  "ast_kind": "function_item",
  "visibility": "pub",
  "start_line": 40,
  "end_line": 96,
  "byte_start": 1200,
  "byte_end": 2480,
  "imports": ["serde", "tokio"],
  "tokens": 180,
  "hash": "stable-content-hash",
  "branch": "main",
  "commit": "abc123",
  "code": "..."
}
```

### Recommended payload indexes for Caduceus
Create field indexes on:
- `repo` → `keyword`
- `workspace` → `keyword`
- `file_path` → `keyword`
- `language` → `keyword`
- `symbol_kind` → `keyword`
- `symbol_name` → `text` or `keyword` (usually both patterns are useful; choose by query style)
- `container` → `keyword`
- `start_line`, `end_line`, `tokens` → `integer`
- `commit` / `branch` → `keyword`

Create through:
- REST: `PUT /collections/{collection_name}/index`
- embedded: `FieldIndexOperations::CreateIndex(CreateIndex { ... })`

Embedded example from `demo.rs`:

```rust
shard.update(UpdateOperation::FieldIndexOperation(
    FieldIndexOperations::CreateIndex(CreateIndex {
        field_name: "language".try_into().unwrap(),
        field_schema: Some(PayloadFieldSchema::FieldType(PayloadSchemaType::Keyword)),
    }),
))?;
```

### Example filters for code search

#### Restrict to one repo and language
```json
{
  "must": [
    { "key": "repo", "match": { "keyword": "caduceus" } },
    { "key": "language", "match": { "keyword": "rust" } }
  ]
}
```

#### Restrict to file path
```json
{
  "must": [
    { "key": "file_path", "match": { "keyword": "src/indexer.rs" } }
  ]
}
```

#### Restrict to functions only
```json
{
  "must": [
    { "key": "symbol_kind", "match": { "keyword": "function" } }
  ]
}
```

#### Exclude tests
```json
{
  "must_not": [
    { "key": "file_path", "match": { "text": "/tests/" } }
  ]
}
```

#### Restrict to line range / chunk size
```json
{
  "must": [
    { "key": "tokens", "range": { "gte": 40, "lte": 300 } }
  ]
}
```

---

## 9. Performance model

### Plain vs HNSW
From `lib/segment/src/types.rs:630+`:
- `Plain {}` = exact full scan, 100% precision, slower on large collections
- `Hnsw(HnswConfig)` = approximate graph index, much faster on larger collections

### Search parameters
From `lib/segment/src/types.rs:591+`:
- `hnsw_ef`: larger beam => better recall, slower search
- `exact`: bypass approximation for exact search
- `quantization`: quantized search tuning
- `indexed_only`: search only indexed/small segments
- `acorn`: filter-aware HNSW optimization

### HNSW config
From `lib/segment/src/types.rs:662+`:
- `m`
- `ef_construct`
- `full_scan_threshold`
- `max_indexing_threads`
- `on_disk`
- `payload_m`
- `inline_storage`

Defaults (`types.rs:1341+`):
- `m = 16`
- `ef_construct = 100`
- `full_scan_threshold = 10_000`
- `on_disk = false`

Behavior:
- small candidate sets may use full scan instead of HNSW
- large segments are optimized into HNSW
- `exact = true` can be used for quality-sensitive reranking/debugging

### Embedded optimize cycle
`EdgeShard::optimize()` (`lib/edge/src/optimize.rs`) is **manual, synchronous, blocking**. It runs:
- merge optimizer
- indexing optimizer
- vacuum optimizer
- config-mismatch optimizer

This is a key operational difference from server mode.

**For Caduceus embedded use:**
- bulk upsert chunks
- then call `optimize()`
- then serve queries
- re-run optimize periodically after large ingest batches

### On-disk vs RAM
#### Payload storage
`PayloadStorageType::from_on_disk_payload()` (`types.rs:1385`) maps:
- `on_disk_payload = true` → `Mmap`
- `on_disk_payload = false` → `InRamMmap`

#### Vector storage
From `types.rs:1535+`:
- `Memory`
- `Mmap`
- `ChunkedMmap`
- `InRamChunkedMmap`
- `InRamMmap`

In edge user config, vectors are controlled via `on_disk: bool` on `EdgeVectorParams`.

Practical guidance:
- Small/medium repos: vectors in RAM, payload on disk or RAM
- Large mono-repos: vectors on disk (`on_disk: true`) + quantization + payload indexes
- Store raw code payload on disk if retaining full source text in payload

---

## 10. Persistence, WAL, snapshots

### WAL
`EdgeShard` uses `SerdeWal<CollectionUpdateOperations>` (`lib/edge/src/lib.rs`).

Default WAL options (`lib/edge/src/lib.rs:188`):
- segment capacity: `32 MiB`
- segments ahead: `0`
- retain closed: `1`

Qdrant README also explicitly advertises write-ahead logging for persistence.

### Flush semantics
`EdgeShard::flush()` flushes both:
- WAL
- all segments

`Drop` for `EdgeShard` calls `flush()` automatically.

### On-disk layout
Derived from `lib/edge/src/lib.rs`, `config/shard.rs`, and shard file conventions:

```text
<db-path>/
  edge_config.json
  wal/
  segments/
```

### Snapshots
Embedded snapshot API (`lib/edge/src/snapshots.rs`):
- `snapshot_manifest()`
- `recover_partial_snapshot(...)`
- `unpack_snapshot(...)`

Use snapshots for backup/export/recovery workflows.

### Reloading persisted local DB
`lib/edge/publish/examples/src/bin/load-existing.rs` demonstrates:
- create/populate shard
- drop it
- reload with `EdgeShard::load(path, None)`
- read back points successfully

So embedded persistence is intended to survive process restarts.

---

## 11. Configuration knobs Caduceus should care about

### Embedded config
`EdgeConfig` (`lib/edge/src/config/shard.rs`):
- `on_disk_payload: bool`
- `vectors: HashMap<String, EdgeVectorParams>`
- `sparse_vectors: HashMap<String, EdgeSparseVectorParams>`
- `hnsw_config: HnswConfig`
- `quantization_config: Option<QuantizationConfig>`
- `optimizers: EdgeOptimizersConfig`

### Per-vector config
`EdgeVectorParams` (`lib/edge/src/config/vectors.rs`):
- `size`
- `distance`
- `on_disk`
- `multivector_config`
- `datatype`
- `quantization_config`
- `hnsw_config`

### Optimizer config
`EdgeOptimizersConfig` (`lib/edge/src/config/optimizers.rs`):
- `deleted_threshold`
- `vacuum_min_vector_number`
- `default_segment_number`
- `max_segment_size`
- `indexing_threshold`
- `prevent_unoptimized`

Operational meaning:
- lower `indexing_threshold` => HNSW kicks in sooner
- smaller `max_segment_size` => more segments, faster incremental maintenance, more overhead
- `deleted_threshold` / `vacuum_min_vector_number` control compaction/vacuum

### Search-time config
`SearchParams` (`lib/segment/src/types.rs:591+`):
- use `hnsw_ef` to tune recall/latency
- use `exact` for evaluation/debugging or very small datasets
- use `indexed_only` to avoid slow searches during incomplete indexing
- use quantization rescoring when quality matters

---

## 12. Best practices for semantic code search in Caduceus

### Recommended storage design
1. **One collection/shard per workspace or repo-set**
   - simpler filtering and isolation
2. **Use named vectors** if you plan multiple embedding spaces
   - e.g. `text`, `symbol`, `docstring`
3. **Store AST-aligned chunks as points**
   - function, method, class, module block, trait impl, doc-comment block
4. **Keep rich payload metadata**
   - enough to reconstruct exact source context without another expensive scan

### Recommended chunking
Not specified by Qdrant itself, but best aligned with its model:
- chunk by tree-sitter semantic unit, not arbitrary token windows
- aim for roughly **40–300 tokens** per chunk for most code embeddings
- use separate chunks for:
  - top-level symbols
  - methods/functions
  - large doc comments
  - important constant/config blocks
- preserve parent/container metadata in payload

### Recommended payload schema for IDE use
At minimum:
- `repo`
- `file_path`
- `language`
- `symbol_kind`
- `symbol_name`
- `container`
- `start_line`
- `end_line`
- `hash`
- `code`

Optional but useful:
- `imports`
- `visibility`
- `ast_kind`
- `branch`
- `commit`
- `embedding_model`
- `chunk_version`

### Retrieval strategy
1. Embed query
2. Filter by repo/workspace/language when available
3. Search top-K semantically (`limit ~20-50`)
4. Optional second-stage rerank using:
   - `query()` prefetch/fusion/MMR
   - or application-side reranking
5. Expand to neighboring chunks in the application if needed

### Good default config for a local IDE database
For a moderate codebase:
- distance: `Cosine`
- datatype: `Float32` initially
- payload on disk: `true`
- vectors on disk: `false` initially
- HNSW: defaults are fine to start
- create payload indexes on `repo`, `file_path`, `language`, `symbol_kind`
- bulk ingest then run `optimize()`

For very large local indexes:
- consider `Float16`
- consider vector `on_disk: true`
- consider scalar/product quantization in server mode; be more conservative in embedded edge mode
- keep `code` payload on disk

### Recommended exact/approx behavior
- During tests/evaluation: use `exact=true` to validate semantic quality
- In production IDE retrieval: use HNSW default approximate search
- For high-stakes actions (e.g. auto-edit selection), optionally rerun exact search on narrowed candidates

---

## 13. Rust client situation

The main repo README lists an official **Rust client** as a separate repository (`README.md:74+`), but the client implementation is **not** in this source tree.

Inside this repo, the Rust-facing APIs are:
- the **server API types** (`lib/api/**`) used to build REST/gRPC services
- the **embedded `qdrant-edge` API** (`lib/edge/**`)

For Caduceus’s “embed natively” requirement, the most relevant Rust API surface in this repo is **`qdrant-edge`**, not the external network client.

---

## 14. Final recommendations for Caduceus

### If you want no separate process
Use **`qdrant-edge`** and build around `EdgeShard`.

Best operational pattern:
1. open/load local shard from workspace path
2. configure one dense vector field sized to your embedding model
3. upsert AST-derived chunks as points with rich payload
4. create payload indexes for `repo`, `file_path`, `language`, `symbol_kind`
5. bulk ingest, then `optimize()`
6. serve semantic search through `query()` (or `search()` for simple nearest-neighbor)
7. `flush()` on important checkpoints; rely on WAL + persisted segments for restart safety

### If you can afford a background service
Use the server REST/gRPC API, and prefer:
- `POST /collections/{collection_name}/points/query` over deprecated `/search`
- indexed payload filters to keep search targeted
- on-disk payloads for large source-text payloads

### Suggested first-pass Caduceus schema
- collection: `code_search`
- vector: `text` size = embedding dimension, distance = `Cosine`
- payload indexes: `repo`, `file_path`, `language`, `symbol_kind`, `start_line`, `end_line`
- payload body: code snippet + exact source coordinates + symbol metadata

This matches Qdrant’s strengths: vector similarity + structured filtering + durable local storage.
