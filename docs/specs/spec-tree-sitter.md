# Tree-sitter Behavioral Specification (Rust-focused)

Scope analyzed:
- `lib/include/tree_sitter/api.h` (C ABI contract)
- `lib/src/*.c,*.h` (runtime behavior)
- `lib/binding_rust/lib.rs`, `lib/binding_rust/wasm_language.rs` (Rust API)
- `crates/highlight`, `crates/tags`, `crates/loader`, `crates/language`, `crates/generate`
- `docs/src/*` (official behavior docs)

---

## 0) Executive model for Caduceus

Tree-sitter is a GLR-based incremental parser that:
- Produces concrete syntax trees (CSTs) with byte + row/column spans.
- Reuses unchanged subtrees across edits (`old_tree` + `ts_tree_edit`).
- Preserves useful structure even under syntax errors (`ERROR`, `MISSING`).
- Exposes query-based structural matching for symbols/highlights/navigation.

For AST-chunk embedding/search, the reliable extraction pipeline is:
1. Parse file -> `Tree`.
2. Find semantic nodes via language-specific queries (`tags.scm`, custom chunk query).
3. Use node byte ranges to slice source text.
4. On edit: apply `InputEdit`, reparse incrementally with old tree, refresh only changed ranges.

---

## 1) Core API behavior

## 1.1 Core C concepts

Primary opaque handles:
- `TSParser`, `TSTree`, `TSNode`, `TSTreeCursor`
- `TSLanguage` (generated grammar)
- `TSQuery`, `TSQueryCursor`
- `TSLookaheadIterator`

Coordinates:
- Byte offsets and `TSPoint {row, column}` are both first-class.
- Rows/columns are zero-based.
- Newline model in docs: line-feed (`\n`) boundaries.

Language compatibility:
- Library constants: `TREE_SITTER_LANGUAGE_VERSION=15`, `TREE_SITTER_MIN_COMPATIBLE_LANGUAGE_VERSION=13`.
- Parser rejects incompatible ABI language versions.

## 1.2 Parser lifecycle

Creation/destruction:
- `ts_parser_new`, `ts_parser_delete`

Language:
- `ts_parser_set_language` returns `false` on ABI mismatch.
- `ts_parser_language` gets active language.

Input styles:
- `ts_parser_parse` with callback-backed `TSInput`
- `ts_parser_parse_string`
- `ts_parser_parse_string_encoding` (UTF8/UTF16LE/UTF16BE)
- Custom decode path via `TSInputEncodingCustom` + `TSDecodeFunction`

Included ranges:
- `ts_parser_set_included_ranges` restricts parse to disjoint ordered ranges.
- Parser copies the ranges; caller keeps ownership of input array.
- Invalid order/overlap returns `false`.

Observability/control:
- Logger via `ts_parser_set_logger`
- DOT graph debug output via `ts_parser_print_dot_graphs`
- Parse cancellation/progress via `ts_parser_parse_with_options` and callback.

Reset semantics:
- `ts_parser_reset` clears resumable parse state.
- Important after cancellation when parser should switch documents.

## 1.3 Tree + node traversal

Tree:
- `ts_tree_root_node`, `ts_tree_root_node_with_offset`
- `ts_tree_edit`, `ts_tree_get_changed_ranges`
- `ts_tree_included_ranges`
- `ts_tree_copy` is cheap (refcounted subtree retention)

Node:
- Type/grammar type: `ts_node_type`, `ts_node_grammar_type`, ids/symbols
- Position/range: start/end bytes and points
- Structural flags: named/extra/missing/error/has_error/has_changes
- Navigation: parent/siblings/children (named + all)
- Field access: by name and id
- Descendant search: byte and point ranges

Cursor:
- `ts_tree_cursor_new` rooted at any node
- Cannot move above cursor root
- Fast imperative traversal APIs
- Some operations explicitly slower (`goto_previous_sibling`, `goto_last_child`)

---

## 2) Incremental parsing: edit protocol + performance behavior

## 2.1 Required edit protocol

To incrementally reparse correctly:
1. Build `InputEdit` with exact old/new byte and point boundaries.
2. Apply to previous tree: `tree.edit(&edit)` / `ts_tree_edit`.
3. Reparse with old tree: `parse(new_text, Some(&old_tree))`.

If holding previously fetched nodes across edits:
- Must also call `node.edit(&edit)`/`ts_node_edit` on those retained node handles.

`InputEdit` invariants (must hold):
- `start <= old_end` in byte and point space.

## 2.2 What is reused

Runtime keeps a `ReusableNode` stream from old tree (`parser.c`):
- Reuses unchanged subtrees/tokens when lexical state + parse context permit.
- Reuse blocked by fragility, scanner-state mismatch, token incompatibility, etc.
- Included-range differences are tracked and affect reuse windows.

## 2.3 Cost model and recovery impact

Error-cost constants (`lib/src/error_costs.h`):
- `ERROR_COST_PER_RECOVERY = 500`
- `ERROR_COST_PER_MISSING_TREE = 110`
- `ERROR_COST_PER_SKIPPED_TREE = 100`
- `ERROR_COST_PER_SKIPPED_LINE = 30`
- `ERROR_COST_PER_SKIPPED_CHAR = 1`

Parser tracks multiple stack versions, condenses by cost, and can stop early when a finished tree is cheaper than any in-progress version.

## 2.4 Progress callback behavior

Parser checks progress callback every `OP_COUNT_PER_PARSER_CALLBACK_CHECK = 100` operations (`parser.c`).
- Callback returning break/true cancels parse.
- Parse can be resumed unless parser is reset.

## 2.5 Changed ranges semantics

`ts_tree_get_changed_ranges(old,new)` returns structural-difference ranges:
- Ancestor-chain differences from root to leaves.
- May be conservative (slightly larger than minimal exact text diff).
- Use this to limit downstream re-index/query/chunk refresh.

---

## 3) Query system specification

## 3.1 Query language capabilities

Pattern language (S-expression) supports:
- Node type matching, nested child constraints
- Field constraints (`name: (...)`)
- Negated fields (`!type_parameters`)
- Anonymous tokens (`"if"`, `"+"`)
- Wildcards (`_`, `(_)`)
- `ERROR`, `MISSING`, specific missing token matches
- Supertypes (`(expression)`, `(expression/binary_expression)`)
- Captures (`@name`)
- Quantifiers (`+`, `*`, `?`)
- Grouped sibling sequences
- Alternations (`[...]`)
- Anchors (`.` before/after/between children)

## 3.2 Predicates/directives

Standard predicate/directive families documented + parsed by Rust binding:
- `eq?`, `not-eq?`, `any-eq?`, `any-not-eq?`
- `match?`, `not-match?`, `any-match?`, `any-not-match?`
- `any-of?`, `not-any-of?`
- `is?`, `is-not?`
- `set!`
- others become `general_predicates`

Important: C core exposes predicate steps structurally; higher layers implement semantics.
Rust binding implements common text predicates directly in `QueryMatch::satisfies_text_predicates`.

## 3.3 Query execution behavior

`TSQuery`:
- Immutable and thread-shareable.
- Compilation errors include offset + kind (`Syntax`, `NodeType`, `Field`, `Capture`, `Structure`, `Language`).

`TSQueryCursor`:
- Stateful executor (not immutable query object).
- Two traversal modes:
  - by match (`next_match` / Rust `matches`)
  - by capture order (`next_capture` / Rust `captures`)

Range controls:
- Intersecting ranges (`set_byte_range` / `set_point_range`): match returned if any overlap.
- Containing ranges (`set_containing_*`): match returned only if fully contained.

Backpressure controls:
- `match_limit`: cap in-progress matches.
- If exceeded, earliest-starting match can be dropped.
- `did_exceed_match_limit` exposes this condition.

Depth control:
- `set_max_start_depth` limits root-start search depth (`None` = unlimited).

---

## 4) Rust crate API (exhaustive public surface for core types)

Crate: `lib/binding_rust` (`tree-sitter`)

## 4.1 Core exported constants/types

- ABI constants: `LANGUAGE_VERSION`, `MIN_COMPATIBLE_LANGUAGE_VERSION`
- Core structs: `Language`, `LanguageRef`, `Parser`, `Tree`, `Node`, `TreeCursor`, `Query`, `QueryCursor`, `QueryMatch`, `QueryCapture`, `Point`, `Range`, `InputEdit`
- Progress wrappers: `ParseState`, `QueryCursorState`, `ParseOptions`, `QueryCursorOptions`
- Query metadata: `CaptureQuantifier`, `QueryProperty`, `QueryPredicate`, `QueryPredicateArg`
- Errors: `LanguageError`, `IncludedRangesError`, `QueryError`, `QueryErrorKind`
- Text plumbing: `TextProvider`, `LossyUtf8`
- Lookahead: `LookaheadIterator`

## 4.2 `Language`

Methods:
- `new(LanguageFn)`
- `name`, `abi_version`, `metadata`
- `node_kind_count`, `parse_state_count`
- `supertypes`, `subtypes_for_supertype`
- `node_kind_for_id`, `id_for_node_kind`
- `node_kind_is_named`, `node_kind_is_visible`, `node_kind_is_supertype`
- `field_count`, `field_name_for_id`, `field_id_for_name`
- `next_state`
- `lookahead_iterator`

Behavior:
- Clone/drop maps to `ts_language_copy/delete` ref semantics.

## 4.3 `Parser`

Methods:
- `new`
- `set_language`, `language`
- `logger`, `set_logger`
- `print_dot_graphs`, `stop_printing_dot_graphs` (`std` non-wasi)
- Parse APIs:
  - `parse`
  - `parse_with_options`
  - `parse_utf16_le`, `parse_utf16_le_with_options`
  - `parse_utf16_be`, `parse_utf16_be_with_options`
  - `parse_custom_encoding<D: Decode,...>`
- `reset`
- `set_included_ranges`, `included_ranges`

Behavior notes:
- `set_language` checks ABI range and returns `LanguageError::Version(...)`.
- Parse returns `Option<Tree>` (`None` on failure/cancel/no language).

## 4.4 `Tree`

Methods:
- `root_node`, `root_node_with_offset`
- `language`
- `edit`
- `walk`
- `changed_ranges`
- `included_ranges`
- `print_dot_graph` (`std` non-wasi)

Traits:
- `Clone` is shallow/cheap.
- `Drop` frees tree.

## 4.5 `Node`

Methods include:
- Identity/type: `id`, `kind_id`, `grammar_id`, `kind`, `grammar_name`, `language`
- Flags: `is_named`, `is_extra`, `is_missing`, `is_error`, `has_error`, `has_changes`
- Parse state: `parse_state`, `next_parse_state`
- Range/position: `start_byte`, `end_byte`, `byte_range`, `range`, `start_position`, `end_position`
- Navigation: `parent`, `child_with_descendant`, sibling APIs
- Child access: `child`, `named_child`, counts
- Field APIs: `child_by_field_name`, `child_by_field_id`, field-name lookup for children
- Iterators via cursor reuse: `children`, `named_children`, `children_by_field_name`, `children_by_field_id`
- Descendant lookup by byte/point (named + unnamed)
- Serialization: `to_sexp`
- Text slicing: `utf8_text`, `utf16_text`
- Cursor start: `walk`
- Node edit: `edit`

## 4.6 `TreeCursor`

Methods:
- Access: `node`, `field_id`, `field_name`, `depth`, `descendant_index`
- Movement: `goto_first_child`, `goto_last_child`, `goto_parent`, `goto_next_sibling`, `goto_previous_sibling`, `goto_descendant`
- Indexed moves: `goto_first_child_for_byte`, `goto_first_child_for_point`
- Reinit: `reset`, `reset_to`

## 4.7 `Query`

Methods:
- Construction: `new`, `new_raw`
- Pattern offsets: `start_byte_for_pattern`, `end_byte_for_pattern`
- Introspection: `pattern_count`, `capture_names`, `capture_quantifiers`, `capture_index_for_name`
- Predicate/property metadata: `property_predicates`, `property_settings`, `general_predicates`
- Runtime controls: `disable_capture`, `disable_pattern`
- Pattern shape checks: `is_pattern_rooted`, `is_pattern_non_local`, `is_pattern_guaranteed_at_step`

Rust-specific behavior:
- Validates/compiles known predicate forms, regexes, argument structure.

## 4.8 `QueryCursor`, `QueryMatch`, iterators

`QueryCursor` methods:
- `new`, `match_limit`, `set_match_limit`, `did_exceed_match_limit`
- Execute/iterate:
  - `matches`, `matches_with_options`
  - `captures`, `captures_with_options`
- Range/depth filters:
  - `set_byte_range`, `set_point_range`
  - `set_containing_byte_range`, `set_containing_point_range`
  - `set_max_start_depth`

`QueryMatch` methods:
- `id`, `remove`, `nodes_for_capture_index`
- predicate evaluation helper: `satisfies_text_predicates`

Iterator model:
- Uses `StreamingIterator`/`StreamingIteratorMut` for safety over C-owned moving buffers.

## 4.9 Lookahead + completion primitives

`LookaheadIterator`:
- `language`, `current_symbol`, `current_symbol_name`
- `reset`, `reset_state`, `iter_names`
- Implements `Iterator<Item=u16>` and name iterator

Use with:
- `Language::next_state(node.parse_state(), node.grammar_id())`
- Good for completion/error-node valid-symbol suggestions.

## 4.10 Wasm support (feature-gated)

From `wasm_language.rs`:
- `WasmStore::new(engine)`
- `WasmStore::load_language(name, wasm_bytes)`
- `WasmStore::language_count`
- `Language::is_wasm`
- `Parser::set_wasm_store`, `Parser::take_wasm_store`
- Errors: `WasmError`, `WasmErrorKind`

Contract:
- Wasm language requires parser with compatible wasm store/engine context.

---

## 5) Language grammars: definition + runtime loading

## 5.1 Grammar authoring model

Grammar DSL (`grammar.js`) supports:
- `seq`, `choice`, `repeat`, `repeat1`, `optional`
- Precedence and associativity: `prec`, `prec.left`, `prec.right`, `prec.dynamic`
- Token controls: `token`, `token.immediate`
- Naming/shape: `alias`, `field`
- Contextual words: `word`, `reserved`, `reserved(...)`
- Grammar-wide knobs: `extras`, `inline`, `conflicts`, `externals`, `precedences`, `supertypes`

External scanners:
- Custom `src/scanner.c` with required create/destroy/serialize/deserialize/scan entrypoints.
- Scanner state must serialize fully to support incremental edits and ambiguity handling.

## 5.2 Generator pipeline

CLI `tree-sitter generate`:
- Evaluates `grammar.js` via JS runtime.
- Produces parser C (`src/parser.c`) + node types metadata.
- `crates/generate` contains grammar transforms and table construction.

## 5.3 Runtime loading options

Rust-native options:
1. Static crate dependency (typical):
   - `tree-sitter-<lang>` exposes `LANGUAGE` function pointer object.
   - Convert via `Language::new`/`into` from `LanguageFn`.
2. Dynamic loader crate:
   - `tree-sitter-loader` discovers grammar repos, compiles parser libs, loads symbols.
   - Matches by file extension, first-line regex, content regex, injection regex.
3. Wasm grammar loading:
   - load `.wasm` grammar bytes via `WasmStore::load_language`.

---

## 6) Highlighting and tags

## 6.1 `tree-sitter-highlight`

Key API:
- `HighlightConfiguration::new(language, name, highlights_query, injections_query, locals_query)`
- `configure(recognized_names)`
- `Highlighter::new`, `highlight(...) -> Iterator<Result<HighlightEvent>>`
- `HtmlRenderer` for HTML output

Events:
- `Source {start,end}`
- `HighlightStart(Highlight)`
- `HighlightEnd`

Behavioral notes:
- Merges highlight/injection/locals query logic.
- Supports injection layers and combined injections.
- Uses cancellation polling.
- Designed for one highlighter per thread; configuration is immutable/shareable.

## 6.2 `tree-sitter-tags`

Key API:
- `TagsConfiguration::new(language, tags_query, locals_query)`
- `TagsContext::new`
- `generate_tags(config, source, cancellation_flag) -> (iterator, has_error)`

Tag model:
- `Tag { range, name_range, line_range, span, utf16_column_range, docs, is_definition, syntax_type_id }`
- Capture conventions: `@definition.*`, `@reference.*`, `@name`, optional `@doc`.
- Supports doc cleanup via query directives (`strip!`, `select-adjacent!`).

---

## 7) Error recovery and partial parses

Tree-sitter guarantees useful trees for invalid code:
- Explicit error nodes (`ERROR`).
- Inserted zero-width missing nodes (`MISSING ...`).
- Error-aware flags on nodes (`has_error`, `is_error`, `is_missing`).

Runtime strategy (`parser.c`):
- Uses costed recovery strategies:
  1. recover to previous viable state (wrap skipped region in `ERROR`)
  2. wrap current lookahead in `ERROR` and continue
- Maintains multiple stack versions, prunes by error cost and redundancy.
- EOF recovery wraps remaining content in an `ERROR` node if needed.

Practical implication for indexing:
- You can keep chunk extraction active during live typing.
- Exclude or down-rank chunks intersecting `ERROR`/`MISSING` if desired.

---

## 8) Memory, ownership, concurrency

## 8.1 C-level

- Trees are refcounted through subtrees (`SubtreeHeapData.ref_count`).
- `ts_tree_copy` retains underlying root; cheap shallow clone.
- `ts_tree_delete` releases retained structure.
- Query objects immutable; query cursors mutable execution state.
- Trees are not thread-safe for concurrent mutation; copy before cross-thread simultaneous use.

## 8.2 Rust-level

Ownership wrappers around C pointers with `Drop`.
- `Tree` clone is cheap (C copy semantics).
- `Query` immutable metadata cached in Rust side.
- Streaming iterators prevent invalid copying of moving C match buffers.

`unsafe impl Send+Sync` exists for major wrappers (`Language`, `Parser`, `Tree`, `Node`, `Query`, `QueryCursor`, `TreeCursor`, `LookaheadIterator`). In practice, follow conservative usage:
- Per-thread parser/cursor instances.
- Share immutable `Language`/`Query` freely.
- Clone trees for parallel processing branches.

---

## 9) Integration patterns (editors and IDEs)

## 9.1 Canonical architecture pattern

Shared pattern across modern editors:
1. Parser-per-buffer (or worker) with language set.
2. Keep latest `Tree` snapshot.
3. On text edit: compute `InputEdit`, edit tree, incremental reparse with old tree.
4. Compute changed ranges and invalidate only impacted semantic/highlight regions.
5. Run query sets for:
   - highlights (`highlights.scm`)
   - injections (`injections.scm`)
   - locals (`locals.scm`)
   - symbols/tags (`tags.scm` / custom outline queries)

## 9.2 Neovim

From `:help treesitter`:
- Parsers discovered on runtimepath (`parser/{lang}.*`), including `.wasm` when enabled.
- Queries loaded from runtimepath `queries/<lang>/*.scm`.
- Supports query predicates/directives, query modelines (`extends`, `inherits`).
- Highlighting and many text features are query-driven.

## 9.3 Helix

From Helix docs:
- Injection queries are first-class; standard + Helix-specific captures/settings.
- Uses query-driven highlight + injection composition (`@injection.content`, language selectors).

## 9.4 Zed

From Zed’s Tree-sitter architecture writeup:
- Uses CST + incremental parsing for low-latency editing.
- Uses tree queries for highlighting, outline extraction, indentation, syntax-aware selection.
- Emphasizes language-agnostic feature implementation via parser + query packs.

---

## 10) Available grammar ecosystem and maturity

## 10.1 Official upstream organization grammars (from docs)

Agda, Bash, C, C++, C#, CSS, Embedded Template (ERB/EJS), Go, Haskell, HTML, Java, JavaScript, JSDoc, JSON, Julia, OCaml, PHP, Python, Regex, Ruby, Rust, Scala, TypeScript, Verilog.

## 10.2 Discovery surface

- Tree-sitter docs link to official list and wiki list-of-parsers.
- `tree-sitter-loader` can discover local grammar repos (`tree-sitter-*`) and compile/load dynamically.

## 10.3 Practical maturity rubric (for Caduceus)

Tree-sitter itself does not publish a universal maturity score. Use operational signals per grammar:
- Query completeness (`highlights`, `locals`, `injections`, `tags` present?)
- External scanner stability
- Test corpus size / CI health
- Release cadence + issue responsiveness
- Real-editor adoption (Neovim/Helix/Zed extension availability)

Recommended tiers:
- Tier A: parser + full query suite + active maintenance
- Tier B: parser + partial queries
- Tier C: parser only / experimental

---

## 11) Caduceus implementation guidance (Rust-native AST chunking)

## 11.1 Chunk extraction strategy

Prefer query-defined semantic chunks over raw node-type heuristics.

Per language, define chunk query captures like:
- `@chunk.function`
- `@chunk.method`
- `@chunk.class`
- `@chunk.interface`
- `@chunk.module`
- optional `@chunk.doc`
- optional `@chunk.name`

Then:
1. Parse -> root node.
2. Execute query via `QueryCursor::matches`.
3. For each match, extract captured node ranges (`byte_range`).
4. Slice source bytes and store:
   - language
   - node kind
   - stable path/symbol name
   - byte + point ranges
   - error flags (`has_error`, `is_missing` descendants if needed)

## 11.2 Incremental re-index algorithm

On each buffer edit:
1. Build `InputEdit` from rope/piece-table delta.
2. `old_tree.edit(&edit)`.
3. `new_tree = parser.parse(new_text, Some(&old_tree))`.
4. `changed = old_tree.changed_ranges(&new_tree)`.
5. Re-run chunk query only in changed/intersecting regions.
6. Preserve chunk IDs for untouched ranges.

## 11.3 Query execution tuning

- Reuse `Query` globally (immutable/shareable).
- Reuse `QueryCursor` per worker thread.
- Apply `set_byte_range` for localized updates.
- Use `set_containing_*` when requiring full containment.
- Monitor `did_exceed_match_limit`; raise limit if dropping matches.
- Use `set_max_start_depth` for subtree-focused extraction.

## 11.4 Error-tolerant indexing policy

Recommended defaults:
- Index valid chunks even when file has syntax errors.
- Mark chunk quality metadata:
  - `contains_error_node`
  - `contains_missing_node`
  - parse_error_density
- Down-rank but do not discard by default.

## 11.5 Multi-language / injection

For embedded code:
- Either orchestrate explicit included-range parsing per language,
- Or consume injection queries (`injections.scm`) and recursively parse captured regions.

Store parent-child chunk lineage for cross-language search provenance.

---

## 12) Gotchas and hard constraints

- Must apply exact edit deltas before incremental reparse; mismatched edits break reuse/changed-range accuracy.
- `TSNode` handles cached before edit require `node.edit` if reused.
- Included ranges must be ordered and non-overlapping.
- Query predicates/directives are partly host-implemented semantics.
- External scanners must serialize full state for correct incremental behavior.
- ABI compatibility must be enforced when loading grammars.

---

## 13) Minimal Rust reference skeleton for Caduceus

```rust
use tree_sitter::{InputEdit, Parser, Query, QueryCursor, Point};
use streaming_iterator::StreamingIterator;

// setup
let mut parser = Parser::new();
parser.set_language(&language)?;
let query = Query::new(&language, chunk_query_source)?;
let mut qc = QueryCursor::new();

// first parse
let mut tree = parser.parse(source_bytes, None).expect("parse failed");

// query chunks
let mut matches = qc.matches(&query, tree.root_node(), source_bytes);
while let Some(m) = matches.next() {
    let m = m; // QueryMatch
    for cap in m.captures {
        let node = cap.node;
        let bytes = &source_bytes[node.byte_range()];
        // emit chunk
    }
}

// incremental edit
let edit = InputEdit {
    start_byte, old_end_byte, new_end_byte,
    start_position: Point::new(start_row, start_col),
    old_end_position: Point::new(old_end_row, old_end_col),
    new_end_position: Point::new(new_end_row, new_end_col),
};

tree.edit(&edit);
let new_tree = parser.parse(new_source_bytes, Some(&tree)).expect("reparse failed");
let changed: Vec<_> = tree.changed_ranges(&new_tree).collect();
```

---

## 14) Source anchors (primary)

- Core C ABI: `lib/include/tree_sitter/api.h`
- Runtime internals: `lib/src/parser.c`, `lib/src/tree.c`, `lib/src/subtree.c`, `lib/src/error_costs.h`
- Rust API: `lib/binding_rust/lib.rs`, `lib/binding_rust/wasm_language.rs`
- Rust helper crates:
  - highlight: `crates/highlight/src/highlight.rs`
  - tags: `crates/tags/src/tags.rs`
  - loader: `crates/loader/src/loader.rs`
  - language ABI shim: `crates/language/src/language.rs`
- Docs:
  - parsing/walking/queries/static types/ABI versions under `docs/src/using-parsers`
  - grammar DSL and grammar authoring under `docs/src/creating-parsers`
  - highlighting/tags docs: `docs/src/3-syntax-highlighting.md`, `docs/src/4-code-navigation.md`
- Editor integration references:
  - Neovim treesitter help: https://neovim.io/doc/user/treesitter/
  - Zed syntax-aware editing: https://zed.dev/blog/syntax-aware-editing
  - Helix injection guide: https://docs.helix-editor.com/guides/injection.html
