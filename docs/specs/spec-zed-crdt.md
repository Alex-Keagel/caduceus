# Zed CRDT Specification

**Extracted from:** `~/caduceus-reference/zed/`  
**Purpose:** Behavioral specification for building Caduceus multiplayer editing layer  
**Focus:** Real-time collaborative editing with human developers + AI agents

---

## Table of Contents

1. [Architecture Overview](#1-architecture-overview)
2. [clock Crate — Lamport & Vector Clocks](#2-clock-crate--lamport--vector-clocks)
3. [rope Crate — Efficient Text Storage](#3-rope-crate--efficient-text-storage)
4. [sum_tree Crate — B+ Tree Foundation](#4-sum_tree-crate--b-tree-foundation)
5. [text Crate — CRDT Text Buffer Core](#5-text-crate--crdt-text-buffer-core)
6. [language Crate — Buffer with Syntax](#6-language-crate--buffer-with-syntax)
7. [multi_buffer Crate — Multi-File Management](#7-multi_buffer-crate--multi-file-management)
8. [Network Synchronization](#8-network-synchronization)
9. [Implementation Guidance for Caduceus](#9-implementation-guidance-for-caduceus)

---

## 1. Architecture Overview

Zed's CRDT system is a layered architecture designed for real-time collaborative text editing:

```
┌─────────────────────────────────────────────────────────────┐
│                      Editor UI Layer                         │
│              (editor crate - consumes operations)            │
├─────────────────────────────────────────────────────────────┤
│                    MultiBuffer Layer                         │
│        (multi_buffer crate - manages multiple files)         │
├─────────────────────────────────────────────────────────────┤
│                Language-Aware Buffer Layer                   │
│     (language crate - syntax trees, diagnostics, etc.)       │
├─────────────────────────────────────────────────────────────┤
│                   CRDT Text Buffer Layer                     │
│    (text crate - THE CORE - handles concurrent edits)        │
├─────────────────────────────────────────────────────────────┤
│                      Rope Data Structure                     │
│             (rope crate - efficient text storage)            │
├─────────────────────────────────────────────────────────────┤
│                    Sum Tree Foundation                       │
│              (sum_tree crate - B+ tree impl)                 │
├─────────────────────────────────────────────────────────────┤
│                      Clock Primitives                        │
│            (clock crate - Lamport timestamps)                │
└─────────────────────────────────────────────────────────────┘
```

### Key Design Decisions

1. **CRDT Type:** Zed uses a **RGA-based (Replicated Growable Array)** CRDT with Lamport timestamps
2. **Tombstone Model:** Deleted text is preserved in `deleted_text` rope for undo/redo
3. **Fragment-Based:** Text is broken into `Fragment` units, each with unique identity
4. **Anchor System:** Positions are represented as `Anchor` types that remain stable across edits
5. **Version Vectors:** `clock::Global` tracks what each replica has observed

---

## 2. clock Crate — Lamport & Vector Clocks

**Location:** `crates/clock/src/clock.rs`

### ReplicaId

Unique identifier for each collaborator:

```rust
pub struct ReplicaId(u16);

impl ReplicaId {
    pub const LOCAL: ReplicaId = ReplicaId(0);           // Host user
    pub const REMOTE_SERVER: ReplicaId = ReplicaId(1);   // SSH remote
    pub const AGENT: ReplicaId = ReplicaId(2);           // AI agent
    pub const LOCAL_BRANCH: ReplicaId = ReplicaId(3);    // Branch buffer
    pub const FIRST_COLLAB_ID: ReplicaId = ReplicaId(8); // First collaborator
}
```

**For Caduceus:** AI agents should use `ReplicaId::AGENT` or allocated IDs >= `FIRST_COLLAB_ID`.

### Lamport Timestamp

Total ordering for operations:

```rust
pub type Seq = u32;

pub struct Lamport {
    pub value: Seq,           // Monotonically increasing counter
    pub replica_id: ReplicaId, // Tie-breaker for concurrent ops
}

impl Lamport {
    pub const MIN: Self = Self { replica_id: ReplicaId(u16::MIN), value: Seq::MIN };
    pub const MAX: Self = Self { replica_id: ReplicaId(u16::MAX), value: Seq::MAX };
    
    pub fn new(replica_id: ReplicaId) -> Self;
    pub fn tick(&mut self) -> Self;           // Increment and return
    pub fn observe(&mut self, timestamp: Self); // Update clock on receipt
    
    // Packing for network/storage
    pub fn as_u64(self) -> u64 {
        ((self.value as u64) << 32) | (self.replica_id.0 as u64)
    }
}

// Total ordering: first by value, then by replica_id
impl Ord for Lamport {
    fn cmp(&self, other: &Self) -> Ordering {
        self.value.cmp(&other.value)
            .then_with(|| self.replica_id.cmp(&other.replica_id))
    }
}
```

### Global (Version Vector)

Tracks observed operations per replica:

```rust
pub struct Global {
    values: SmallVec<[u32; 4]>, // values[replica_id] = max observed seq
}

impl Global {
    pub fn new() -> Self;
    pub fn get(&self, replica_id: ReplicaId) -> Seq;
    
    // Core operations
    pub fn observe(&mut self, timestamp: Lamport);   // Mark timestamp as seen
    pub fn join(&mut self, other: &Self);            // Union (max of each)
    pub fn meet(&mut self, other: &Self);            // Intersection (min of each)
    
    // Queries
    pub fn observed(&self, timestamp: Lamport) -> bool;
    pub fn observed_all(&self, other: &Self) -> bool;
    pub fn observed_any(&self, other: &Self) -> bool;
    pub fn changed_since(&self, other: &Self) -> bool;
    
    pub fn iter(&self) -> impl Iterator<Item = Lamport>;
}
```

**Serialization:** The `Global` struct can be serialized as a sparse map `{replica_id: seq}`.

---

## 3. rope Crate — Efficient Text Storage

**Location:** `crates/rope/src/`

### Core Structure

```rust
pub struct Rope {
    chunks: SumTree<Chunk>,  // B+ tree of text chunks
}

// Each chunk stores text with precomputed metadata
pub struct Chunk {
    chars: Bitmap,        // Bit i set = byte i is UTF-8 char boundary
    chars_utf16: Bitmap,  // For UTF-16 offset mapping
    newlines: Bitmap,     // Bit i set = byte i is '\n'
    tabs: Bitmap,         // Bit i set = byte i is '\t'
    text: ArrayString<MAX_BASE, u8>,  // Up to 128 bytes
}

// Chunk sizing constants (production)
pub const MIN_BASE: usize = MAX_BASE / 2;  // 64 bytes minimum
pub const MAX_BASE: usize = 128;           // 128 bytes maximum (Bitmap::BITS)
```

### Key Operations

```rust
impl Rope {
    // Construction
    pub fn new() -> Self;
    pub fn from(text: &str) -> Self;
    
    // Modification (returns new rope, immutable)
    pub fn push(&mut self, text: &str);
    pub fn push_front(&mut self, text: &str);
    pub fn append(&mut self, rope: Rope);
    pub fn replace(&mut self, range: Range<usize>, text: &str);
    pub fn slice(&self, range: Range<usize>) -> Rope;
    
    // Queries
    pub fn len(&self) -> usize;                    // Bytes
    pub fn summary(&self) -> TextSummary;
    pub fn max_point(&self) -> Point;             // (row, col) of end
    
    // Coordinate conversions (O(log n))
    pub fn offset_to_point(&self, offset: usize) -> Point;
    pub fn point_to_offset(&self, point: Point) -> usize;
    pub fn offset_to_offset_utf16(&self, offset: usize) -> OffsetUtf16;
    pub fn clip_offset(&self, offset: usize, bias: Bias) -> usize;
    
    // Iteration
    pub fn chars(&self) -> impl Iterator<Item = char>;
    pub fn chars_at(&self, offset: usize) -> impl Iterator<Item = char>;
    pub fn chunks(&self) -> Chunks<'_>;
    pub fn chunks_in_range(&self, range: Range<usize>) -> Chunks<'_>;
}
```

### TextSummary

Aggregate statistics computed incrementally:

```rust
pub struct TextSummary {
    pub len: usize,              // Total bytes
    pub chars: usize,            // Total characters
    pub len_utf16: OffsetUtf16,  // UTF-16 code units
    pub lines: Point,            // {row: newline_count, column: last_line_len_bytes}
    pub first_line_chars: u32,
    pub last_line_chars: u32,
    pub last_line_len_utf16: u32,
    pub longest_row: u32,
    pub longest_row_chars: u32,
}
```

### Point Types

```rust
// Zero-indexed (row, column) in bytes
pub struct Point {
    pub row: u32,
    pub column: u32,
}

// Zero-indexed (row, column) in UTF-16 code units
pub struct PointUtf16 {
    pub row: u32,
    pub column: u32,
}

// UTF-16 offset from start
pub struct OffsetUtf16(pub usize);
```

### Performance Characteristics

| Operation | Complexity |
|-----------|------------|
| `push(text)` | O(n/chunk_size) amortized |
| `slice(range)` | O(log n) |
| `offset_to_point` | O(log n) |
| `point_to_offset` | O(log n) |
| Random access by byte | O(log n) |

---

## 4. sum_tree Crate — B+ Tree Foundation

**Location:** `crates/sum_tree/src/`

### Core Traits

```rust
// Items stored in the tree
pub trait Item: Clone {
    type Summary: Summary;
    fn summary(&self, cx: <Self::Summary as Summary>::Context<'_>) -> Self::Summary;
}

// Aggregated data for subtrees
pub trait Summary: Clone {
    type Context<'a>: Copy;
    fn zero<'a>(cx: Self::Context<'a>) -> Self;
    fn add_summary<'a>(&mut self, summary: &Self, cx: Self::Context<'a>);
}

// Dimensions for seeking
pub trait Dimension<'a, S: Summary>: Clone {
    fn zero(cx: S::Context<'_>) -> Self;
    fn add_summary(&mut self, summary: &'a S, cx: S::Context<'_>);
}
```

### SumTree Structure

```rust
pub struct SumTree<T: Item>(Arc<Node<T>>);

enum Node<T: Item> {
    Internal {
        height: u8,
        summary: T::Summary,
        child_summaries: ArrayVec<T::Summary, {2 * TREE_BASE}>,
        child_trees: ArrayVec<SumTree<T>, {2 * TREE_BASE}>,
    },
    Leaf {
        summary: T::Summary,
        items: ArrayVec<T, {2 * TREE_BASE}>,
        item_summaries: ArrayVec<T::Summary, {2 * TREE_BASE}>,
    },
}

// Branching factor (production: 6, test: 2)
pub const TREE_BASE: usize = 6;
// Max items per node: 12
```

### Bias

Controls cursor positioning for ambiguous locations:

```rust
pub enum Bias {
    Left,   // Attach to character before position
    Right,  // Attach to character after position
}
```

### Key Operations

```rust
impl<T: Item> SumTree<T> {
    pub fn new(cx: Context) -> Self;
    pub fn push(&mut self, item: T, cx: Context);
    pub fn extend(items: impl IntoIterator<Item = T>, cx: Context);
    pub fn append(&mut self, other: Self, cx: Context);
    
    pub fn cursor<D: Dimension>(&self, cx: Context) -> Cursor<T, D>;
    pub fn summary(&self) -> &T::Summary;
    pub fn extent<D: Dimension>(&self, cx: Context) -> D;
    
    // Efficient seek to position by any dimension
    pub fn find<D, Target>(&self, cx: Context, target: &Target, bias: Bias) 
        -> (D, D, Option<&T>);
}
```

---

## 5. text Crate — CRDT Text Buffer Core

**Location:** `crates/text/src/`

This is **the core CRDT implementation**. Understanding this is critical for Caduceus.

### Buffer Structure

```rust
pub struct Buffer {
    snapshot: BufferSnapshot,
    history: History,
    deferred_ops: OperationQueue<Operation>,  // Ops awaiting dependencies
    deferred_replicas: HashSet<ReplicaId>,
    pub lamport_clock: clock::Lamport,
    subscriptions: Topic<usize>,
    edit_id_resolvers: HashMap<clock::Lamport, Vec<oneshot::Sender<()>>>,
    wait_for_version_txs: Vec<(clock::Global, oneshot::Sender<()>)>,
}

pub struct BufferSnapshot {
    visible_text: Rope,           // Currently visible text
    deleted_text: Rope,           // Tombstoned text (for undo)
    fragments: SumTree<Fragment>, // CRDT fragment tree
    insertions: SumTree<InsertionFragment>, // Index by insertion timestamp
    insertion_slices: TreeSet<InsertionSlice>,
    undo_map: UndoMap,
    pub version: clock::Global,   // Current version vector
    remote_id: BufferId,
    replica_id: ReplicaId,
    line_ending: LineEnding,
}
```

### Fragment — The CRDT Unit

Each `Fragment` represents a piece of text with identity:

```rust
struct Fragment {
    id: Locator,                    // Unique position identifier
    timestamp: clock::Lamport,      // When this text was inserted
    insertion_offset: u32,          // Offset within original insertion
    len: u32,                       // Length in bytes
    visible: bool,                  // false = deleted (tombstone)
    deletions: SmallVec<[clock::Lamport; 2]>,  // Timestamps of delete ops
    max_undos: clock::Global,       // Track undo state
}

impl Fragment {
    // Visibility depends on undo state
    fn is_visible(&self, undos: &UndoMap) -> bool {
        !undos.is_undone(self.timestamp) && 
        self.deletions.iter().all(|d| undos.is_undone(*d))
    }
    
    fn was_visible(&self, version: &clock::Global, undos: &UndoMap) -> bool {
        (version.observed(self.timestamp) && !undos.was_undone(self.timestamp, version))
        && self.deletions.iter().all(|d| 
            !version.observed(*d) || undos.was_undone(*d, version)
        )
    }
}
```

### Locator — Fractional Indexing

`Locator` provides stable ordering for fragments without renumbering:

```rust
pub struct Locator(SmallVec<[u64; 2]>);

impl Locator {
    pub const fn min() -> Self;  // [u64::MIN]
    pub const fn max() -> Self;  // [u64::MAX]
    
    // Generate ID between two locators
    pub fn between(lhs: &Self, rhs: &Self) -> Self {
        // Uses fractional indexing with >> 48 shift for sequential typing
        // Ensures: lhs < between(lhs, rhs) < rhs
    }
}
```

**Important:** The `>> 48` shift optimizes for sequential forward typing, keeping locator depth minimal.

### Operation Types

```rust
pub enum Operation {
    Edit(EditOperation),
    Undo(UndoOperation),
}

pub struct EditOperation {
    pub timestamp: clock::Lamport,    // Unique operation ID
    pub version: clock::Global,       // Dependencies (causality)
    pub ranges: Vec<Range<FullOffset>>, // Ranges to delete/replace
    pub new_text: Vec<Arc<str>>,      // Text to insert at each range
}

pub struct UndoOperation {
    pub timestamp: clock::Lamport,
    pub version: clock::Global,
    pub counts: HashMap<clock::Lamport, u32>,  // edit_id -> undo_count
}
```

**Key Insight:** `FullOffset` is the offset including both visible and deleted text, which is stable across concurrent edits.

### Anchor — Stable Positions

```rust
pub struct Anchor {
    timestamp_replica_id: clock::ReplicaId,
    timestamp_value: clock::Seq,
    pub offset: u32,       // Offset within the insertion
    pub bias: Bias,        // Left or Right attachment
    pub buffer_id: BufferId,
}

impl Anchor {
    pub fn min_for_buffer(buffer_id: BufferId) -> Self;
    pub fn max_for_buffer(buffer_id: BufferId) -> Self;
    
    pub fn cmp(&self, other: &Anchor, buffer: &BufferSnapshot) -> Ordering;
    pub fn is_valid(&self, buffer: &BufferSnapshot) -> bool;
}
```

**For Caduceus:** Use `Anchor` for cursor positions, selections, diagnostics, etc. They survive concurrent edits.

### Local Edit Flow

```rust
impl Buffer {
    pub fn edit<R, I, S, T>(&mut self, edits: R) -> Operation
    where
        R: IntoIterator<IntoIter = I>,
        I: ExactSizeIterator<Item = (Range<S>, T)>,
        S: ToOffset,
        T: Into<Arc<str>>,
    {
        self.start_transaction();
        let timestamp = self.lamport_clock.tick();
        let operation = Operation::Edit(self.apply_local_edit(edits, timestamp));
        
        self.history.push(operation.clone());
        self.history.push_undo(operation.timestamp());
        self.snapshot.version.observe(operation.timestamp());
        self.end_transaction();
        operation  // Return for network broadcast
    }
}
```

### Remote Edit Application

```rust
impl Buffer {
    pub fn apply_ops<I: IntoIterator<Item = Operation>>(&mut self, ops: I) {
        let mut deferred_ops = Vec::new();
        for op in ops {
            self.history.push(op.clone());
            if self.can_apply_op(&op) {
                self.apply_op(op);
            } else {
                // Dependencies not yet received
                self.deferred_replicas.insert(op.replica_id());
                deferred_ops.push(op);
            }
        }
        self.deferred_ops.insert(deferred_ops);
        self.flush_deferred_ops();  // Retry deferred ops
    }
    
    fn can_apply_op(&self, op: &Operation) -> bool {
        if self.deferred_replicas.contains(&op.replica_id()) {
            false  // Preserve causal order per replica
        } else {
            self.version.observed_all(match op {
                Operation::Edit(edit) => &edit.version,
                Operation::Undo(undo) => &undo.version,
            })
        }
    }
}
```

### Concurrent Edit Resolution

The key algorithm for resolving concurrent insertions at the same position:

```rust
// In apply_remote_edit:

// Skip over insertions that are concurrent to this edit, but have a 
// HIGHER lamport timestamp (they should appear BEFORE our insertion)
while let Some(fragment) = old_fragments.item() {
    if fragment_start == range.start && fragment.timestamp > timestamp {
        // Concurrent insertion with higher timestamp goes first
        new_ropes.push_fragment(fragment, fragment.visible);
        new_fragments.push(fragment.clone(), &None);
        old_fragments.next();
    } else {
        break;
    }
}
```

**Resolution Rule:** When two replicas insert at the same position concurrently:
1. Compare Lamport timestamps
2. **Higher timestamp wins position priority** (appears first)
3. Within same timestamp, higher replica_id wins

### Undo/Redo Model

Zed uses a **multi-user undo model** where:
- Each edit has an "undo count"
- Odd undo count = undone
- Even undo count = not undone (including 0)
- Undo counts are tracked per-edit in `UndoMap`

```rust
pub struct UndoMap(SumTree<UndoMapEntry>);

struct UndoMapEntry {
    key: UndoMapKey,   // (edit_id, undo_id)
    undo_count: u32,
}

impl UndoMap {
    pub fn is_undone(&self, edit_id: clock::Lamport) -> bool {
        self.undo_count(edit_id) % 2 == 1
    }
    
    pub fn was_undone(&self, edit_id: clock::Lamport, version: &clock::Global) -> bool {
        // Check undo state at specific version
    }
}
```

### Transaction Model

Transactions group edits for atomic undo:

```rust
pub struct Transaction {
    pub id: TransactionId,          // Lamport timestamp
    pub edit_ids: Vec<clock::Lamport>,
    pub start: clock::Global,       // Version before transaction
}

struct History {
    base_text: Rope,
    operations: TreeMap<clock::Lamport, Operation>,
    undo_stack: Vec<HistoryEntry>,
    redo_stack: Vec<HistoryEntry>,
    transaction_depth: usize,
    group_interval: Duration,       // Default 300ms for grouping
}
```

### Subscriptions

React to buffer changes:

```rust
impl Buffer {
    pub fn subscribe(&mut self) -> Subscription<usize>;
}

// Subscription receives Patch<usize> with Edit<usize>
pub struct Edit<D> {
    pub old: Range<D>,  // Range in old buffer
    pub new: Range<D>,  // Range in new buffer
}
```

---

## 6. language Crate — Buffer with Syntax

**Location:** `crates/language/src/buffer.rs`

Wraps `text::Buffer` with syntax awareness:

```rust
pub struct Buffer {
    text: TextBuffer,           // The CRDT buffer
    branch_state: Option<BufferBranchState>,
    file: Option<Arc<dyn File>>,
    saved_mtime: Option<MTime>,
    saved_version: clock::Global,
    language: Option<Arc<Language>>,
    syntax_map: Mutex<SyntaxMap>,  // Tree-sitter integration
    diagnostics: TreeMap<LanguageServerId, DiagnosticSet>,
    remote_selections: TreeMap<ReplicaId, SelectionSet>,
    capability: Capability,
    // ... more fields
}

pub struct BufferSnapshot {
    pub text: text::BufferSnapshot,
    pub syntax: SyntaxSnapshot,
    diagnostics: TreeMap<LanguageServerId, DiagnosticSet>,
    remote_selections: TreeMap<ReplicaId, SelectionSet>,
    language: Option<Arc<Language>>,
    // ...
}

pub enum Capability {
    ReadWrite,  // Can edit
    Read,       // Toggled to read-only
    ReadOnly,   // Immutable replica
}
```

### Remote Selections

For showing other users' cursors:

```rust
pub remote_selections: TreeMap<ReplicaId, SelectionSet>

struct SelectionSet {
    selections: Vec<Selection<Anchor>>,
    lamport_timestamp: clock::Lamport,
}
```

---

## 7. multi_buffer Crate — Multi-File Management

**Location:** `crates/multi_buffer/src/`

Manages multiple buffers/excerpts in one view:

```rust
pub struct MultiBuffer {
    snapshot: RefCell<MultiBufferSnapshot>,
    buffers: BTreeMap<BufferId, BufferState>,
    diffs: HashMap<BufferId, DiffState>,
    subscriptions: Topic<MultiBufferOffset>,
    singleton: bool,  // True if single buffer
    history: History,
    capability: Capability,
    // ...
}

// Excerpt is a range within a buffer
pub struct Excerpt {
    id: ExcerptId,
    locator: Locator,
    buffer_id: BufferId,
    range: ExcerptRange<text::Anchor>,
    // ...
}
```

---

## 8. Network Synchronization

### Operation Serialization

Operations need to be serialized for network transport. Based on the structures:

```rust
// Suggested protobuf/JSON schema:

message EditOperation {
    LamportTimestamp timestamp = 1;
    GlobalVersion version = 2;
    repeated FullOffsetRange ranges = 3;
    repeated string new_text = 4;
}

message UndoOperation {
    LamportTimestamp timestamp = 1;
    GlobalVersion version = 2;
    map<uint64, uint32> counts = 3;  // edit_id -> undo_count
}

message LamportTimestamp {
    uint32 value = 1;
    uint16 replica_id = 2;
}

message GlobalVersion {
    map<uint16, uint32> observed = 1;  // replica_id -> max_seq
}
```

### Network Test Framework

Zed includes a test network simulation:

```rust
pub struct Network<T: Clone, R: rand::Rng> {
    inboxes: BTreeMap<ReplicaId, Vec<Envelope<T>>>,
    disconnected_peers: HashSet<ReplicaId>,
    rng: R,
}

impl Network {
    pub fn broadcast(&mut self, sender: ReplicaId, messages: Vec<T>);
    pub fn receive(&mut self, receiver: ReplicaId) -> Vec<T>;
    // Simulates out-of-order delivery, duplicates
}
```

### Synchronization Protocol

1. **Initial Sync:** Exchange full buffer state including:
   - `version: clock::Global`
   - `visible_text: Rope`
   - `fragments: Vec<Fragment>` (or reconstruct from ops)

2. **Incremental Sync:**
   - Broadcast `Operation` after each local edit
   - Apply received operations via `apply_ops()`
   - Deferred ops handled automatically

3. **Reconnection:**
   - Exchange version vectors
   - Request missing operations based on version diff

---

## 9. Implementation Guidance for Caduceus

### Cursor/Selection Synchronization

For AI agent cursors visible to humans:

```rust
// AI agent should broadcast its selections:
struct AgentSelection {
    buffer_id: BufferId,
    selections: Vec<Selection<Anchor>>,  // Use Anchors!
    lamport_timestamp: clock::Lamport,
}

// Human clients render using:
buffer.remote_selections.get(&ReplicaId::AGENT)
```

### Character-by-Character AI Typing

To show AI typing in real-time:

1. AI generates text incrementally
2. For each character/word, call `buffer.edit()` with small range
3. Broadcast resulting `Operation` immediately
4. Human sees character appear via subscription callback

### Handling Conflicting Edits

When human edits overlap with AI edit:

1. Both edits get unique Lamport timestamps
2. Higher timestamp wins position priority
3. Both texts are preserved (no data loss)
4. Human may see text reorder briefly

**Recommendation:** AI should avoid editing same lines as human cursor.

### Recommended Anchor Usage

```rust
// DON'T use raw offsets (break under concurrent edits):
let cursor_offset: usize = 42;

// DO use Anchors (survive concurrent edits):
let cursor: Anchor = buffer.anchor_before(42);
// or
let cursor: Anchor = buffer.anchor_after(42);

// Later, resolve to current offset:
let current_offset = buffer.offset_for_anchor(&cursor);
```

### Buffer State Machine

```
┌─────────────┐  edit()   ┌─────────────┐
│   Clean     │──────────▶│   Dirty     │
│             │◀──────────│             │
└─────────────┘   save()  └─────────────┘
      ▲                         │
      │                         │ apply_ops()
      │      ┌─────────────┐    ▼
      └──────│  Conflict   │◀───┘
             │  (merge)    │
             └─────────────┘
```

### Performance Considerations

1. **Chunk size:** 64-128 bytes provides good balance
2. **Tree branching factor:** 12 (2 × TREE_BASE) keeps tree shallow
3. **Locator depth:** Optimized for sequential typing (usually depth 1-2)
4. **Parallel processing:** Rope supports parallel construction for large files

### Thread Safety

- `Buffer` is **not thread-safe** internally
- Use `gpui::Entity<Buffer>` for safe concurrent access
- `BufferSnapshot` is **immutable and thread-safe** (Clone is cheap)
- Take snapshots for background syntax parsing

---

## Appendix A: Key Type Reference

| Type | Location | Purpose |
|------|----------|---------|
| `ReplicaId` | clock | Identifies each collaborator |
| `Lamport` | clock | Timestamp for operation ordering |
| `Global` | clock | Version vector |
| `Rope` | rope | Efficient text storage |
| `Point` | rope | (row, column) position |
| `TextSummary` | rope | Aggregate text statistics |
| `SumTree<T>` | sum_tree | B+ tree with summaries |
| `Bias` | sum_tree | Left/Right positioning |
| `Buffer` | text | CRDT text buffer |
| `BufferSnapshot` | text | Immutable buffer state |
| `Fragment` | text | CRDT text unit |
| `Locator` | text | Stable fragment ordering |
| `Anchor` | text | Stable position reference |
| `Operation` | text | Edit or Undo operation |
| `Edit<D>` | text | Range change description |
| `Patch<D>` | text | Collection of edits |

---

## Appendix B: Critical Invariants

1. **Locator ordering:** `Locator::min() < any_locator < Locator::max()`
2. **Fragment uniqueness:** Each `(timestamp, insertion_offset)` pair is unique
3. **Causality:** `op.version` must be observed before applying `op`
4. **Version monotonicity:** `version.observe(ts)` only increases
5. **Anchor validity:** Anchor's timestamp must be in buffer's version
6. **Undo parity:** `undo_count % 2 == 1` means undone
7. **Text preservation:** Deleted text lives in `deleted_text` rope forever

---

## Appendix C: Common Patterns

### Subscribing to Changes

```rust
let subscription = buffer.subscribe();
// In event loop:
while let Some(patch) = subscription.next() {
    for edit in patch.edits() {
        // edit.old = range in previous buffer state
        // edit.new = range in current buffer state
        update_display(edit);
    }
}
```

### Computing Diff Between Versions

```rust
let edits: Vec<Edit<usize>> = buffer.edits_since(&old_version).collect();
// Each edit describes what changed
```

### Getting Text for Range

```rust
let text: String = buffer.text_for_range(start..end).collect();
// or
let rope: Rope = buffer.slice(start..end);
```

---

*End of Specification*
