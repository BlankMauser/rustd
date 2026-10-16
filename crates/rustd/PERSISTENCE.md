# Persisting rustd's analysis state: sizing report

Branch `rustd-persist` (off `rustd-spike`). This is a report with experiments. The code on the
branch is a prototype that exists to produce the numbers below; it is not a feature.

Short version:

- Salsa 0.28.5's `persistence` feature works. A snapshot of the inputs of dx (file texts, source
  roots, crate graph) is 419 MB, saves in 0.7 s and loads in 0.6 s in a fresh process.
- That alone buys nothing: the loader still runs and the expensive queries still run. To skip the
  warm-up, the query results have to be saved, and almost none of their value types can be
  serialized today. On dx that is 129 query ingredients holding 3.5 million memos, most of them
  full of next-solver types, spans and syntax.
- The one derived query I persisted (`parse`) was a loss on both workspaces: rebuilding trees
  from a snapshot costs about as much as parsing.
- CRIU restores a warm dx daemon in 3.4 s (7.5 s with a cold page cache) from an 8.0 GB image
  (1.55 GB with zstd), with no changes to rustd. It answered a sync and rebuilt after an edit.
- Recommendation: process snapshot (CRIU) for restarts on the same machine and same-image VMs,
  the existing machine-code store for everything else, and Salsa persistence only as a slow
  staged project starting at the def-map level, if the first two turn out not to be enough.

All timings are single runs on one 8-core / 31 GB VM (Ubuntu 22.04, kernel 6.8.0-1060-aws) unless
a line says otherwise. Run-to-run noise on the dx cold build is several seconds (the same binary
gave 37.3 s, 42.0 s, 39.3 s, 36.5 s, 37.6 s for the cold build in five runs), so differences of a
few seconds between rows mean nothing.

## 1. What salsa 0.28.5 persistence does

Read from the source of salsa 0.28.5 in the cargo registry (`src/database.rs`, `src/runtime.rs`,
`src/input.rs`, `src/interned.rs`, `src/tracked_struct.rs`, `src/function/memo.rs`,
`src/function.rs`, `src/zalsa.rs`, and the `persist` handling in `salsa-macros`).

0.28.5 is the newest release on crates.io (2026-09-24), and salsa's master after it had no
persistence commits when I looked. The feature landed in salsa PR #967 (2025-08-11), followed by
#973/#975 (flatten dependencies on queries that are not saved), #977, #978, #982 (cycles during
serialization, fixes) and #1151 (reject a zero revision on load). All of those are in 0.28.5.
There is nothing newer to pick up.

### What is written

`(&mut dyn Database).as_serialize()` gives a serde value with two fields.

- `runtime`: the array of current revisions, one per durability. Nothing else of the runtime.
- `ingredients`: a map from ingredient index to that ingredient's data, for ingredients marked
  `persist` only. Struct ingredients come before function ingredients.
  - Input: id -> field values, the revision each field last changed at, the durability of each
    field.
  - Interned: id -> fields, durability, `last_interned_at`.
  - Tracked struct: id -> fields, durability, `updated_at`, per-field revisions. Deleted values
    are left out.
  - Tracked function: key (`"<ingredient index>:<id bits>"`, a string) -> memo: the value,
    `verified_at`, `changed_at`, durability, the origin (assigned / derived / derived-untracked)
    with its edges, `verified_final`, and the ids of tracked structs it created and its cycle
    heads.
- Edges are flattened at save time: an edge to a query that is not persisted is replaced by the
  persisted things that query depends on, down to inputs. So a saved memo can be validated on
  load even if the intermediate queries were not saved.

### What is not written

- Any ingredient without `persist`.
- Memos that have no value. That includes every memo whose value was evicted by `lru = N`.
- Provisional (mid-cycle) memos.
- Accumulators (a TODO in salsa).
- The table pages themselves, the dependency graph of running queries, LRU lists, cancellation
  flags, the event callback.
- Anything outside salsa: in rustd that is the `Files` maps and `CratesMap` in base-db, the VFS
  path <-> `FileId` table, proc-macro clients, and rustd's own `Tree`, listing, rejections and
  machine-code store.

### What the loading side must guarantee

- Same ingredients in the same order. Ingredient indices are assigned by sorting jars by kind
  and type name, and the snapshot refers to ingredients by index. Adding, removing or renaming
  any salsa struct or query in any crate shifts indices. In practice: a snapshot is only valid
  for the exact binary that wrote it. Salsa does not check this; the prototype guards it with
  the size and mtime of the executable, which is crude.
- An empty storage. Ids are restored exactly: on load salsa forces the page and allocates, and
  asserts that the id it gets is the id in the snapshot and that values arrive in allocation
  order. Anything allocated before `deserialize` breaks that. `RootDatabase::new` creates
  singleton inputs, so the prototype builds the database without it.
- Revisions are taken from the snapshot and overwrite the fresh runtime's. Memos keep
  `verified_at`, inputs keep `changed_at`, so a restored memo is valid without re-verification
  as long as nobody sets an input afterwards.
- Durabilities are kept per field and per memo.
- Every dependency of a persisted query must be reachable: an input a persisted query reads
  must itself be persisted (salsa panics otherwise: "the inputs of a persistable tracked
  function must be persistable"). A value created with `specify` must be persistable too.
- The serde format must support borrowed strings (memo keys are read as `&str`) and maps.
  postcard works for the slice here. One thing to check before going further: cycle heads are
  written with `serialize_seq(None)`, which postcard rejects; no memo in the prototype has any.
- Everything outside salsa has to be put back by the embedder, consistent with the restored
  ids. And anything the embedder sets after loading must compare before it sets, or the revision
  moves and restored memos get re-verified or recomputed.

One more thing I noticed and did not measure in isolation: with the feature on, salsa keeps the
edges of memos that only depend on never-changing inputs instead of dropping them. The dx cold
runs with the feature on (43.6 s and 39.1 s build, 9.31 and 8.93 GB peak RSS) are inside the
noise of the runs with it off (36.5 to 42.0 s, 9.02 and 9.03 GB).

## 2. Inventory

### What would have to opt in

Counted from source by `python3 harness/persist_inventory.py` (a regex count of
`#[salsa::input|interned|tracked]` and the interning macros; it can miss macro-generated items):

| crate | inputs | interned | tracked structs | tracked fns |
|---|---:|---:|---:|---:|
| base-db | 8 | 3 | 0 | 3 |
| hir-def | 1 | 18 | 5 | 74 |
| hir-expand | 1 | 1 | 0 | 8 |
| hir-ty | 0 | 5 | 0 | 69 |
| ide-db | 0 | 1 | 0 | 4 |
| span | 0 | 1 | 0 | 0 |
| total | 10 | 29 | 5 | 158 |

What is actually alive after a dx cold build, from salsa's own `memory_usage()`
(`RUSTD_DB_STATS=1`, see the command in section 4, log `dx1-save.log`):

- 69 struct ingredients (inputs, interned, tracked structs, interned query arguments) holding
  2,655,107 values, 98.9 MB of fields not counting their heap.
- 129 query ingredients holding 3,544,706 memos, 264.9 MB of values not counting their heap.
  The process is at about 8.3 GB, so nearly all the weight is on the heap behind those values.

The largest by memo count:

| query | memos | value | class |
|---|---:|---|---|
| `AttrFlags::query_` | 256,749 | bit flags | easy, cheap to recompute |
| `ModuleScope::name_`, `legacy_macro_`, `traits_` | 462,383 | item ids | custom |
| `layout_of_ty_query`, `layout_of_adt_query` | 339,361 | `Arc<Layout>`, keyed by `Ty` | custom (next-solver key) |
| `compiled_instance` | 218,908 | machine code | already has its own store |
| `monomorphized_mir_body_query` (+ closure, promoted) | 238,263 | `Arc<MirBody>` | custom, hard |
| `ImplSignature::of_`, `with_source_map_` | 335,114 | signature + source map | custom; source map: recompute |
| `impl_self_ty`, `impl_trait` | 328,830 | `Ty`, `TraitRef` | custom, hard |
| `HirFileId::ast_id_map_` | 146,488 | `AstIdMap` | recompute |
| `MacroCallId::parse_macro_expansion_` | 139,374 | syntax tree + span map | recompute |
| `file_item_tree_query`, `block_item_tree_query` | 136,327 | `Arc<ItemTree>` | custom |
| `MacroCallId::macro_arg_` | 109,036 | token tree with spans | recompute |
| `TraitImpls::...` | ~92,000 | impl ids by simplified type | custom |
| `GenericPredicates` | 50,713 | clauses | custom, hard |
| `Body::of_`, `with_source_map_` | 77,380 | expression arenas | custom; source map: recompute |
| `InferenceResult::for_body_` | 23,447 | types per expression | custom, hard |
| `mir_body_query` | 23,449 | `Arc<MirBody>` | custom, hard |
| `block_def_map` / `crate_local_def_map` | 20,083 / 713 | `DefMap` | custom, large |
| `EditionedFileId::parse_` | 9,812 | syntax tree | recompute (measured, see 4) |

### Value types without serde

None of base-db, vfs, cfg, intern, span, syntax, tt, hir-expand, hir-def depended on serde
before this branch. What each kind of value needs:

| type | where | class | why |
|---|---|---|---|
| `FileId`, `Edition`, `SourceRootId`, `CfgOptions`, `CrateData`, `Env`, `TargetData`, `VfsPath`, `FileSet` | base-db, vfs, cfg | easy | plain data; done in the prototype with derives |
| `triomphe::Arc<str>`, `Arc<[T]>` | everywhere | easy | needs the `serde` feature of triomphe or a two-line custom fn; done for `FileText` |
| `Symbol` | intern | easy | write the text, intern on read; done. Costs a hash lookup per symbol on load |
| salsa ids (`FunctionId`, `MacroCallId`, `Crate`, ...) | all | easy once persisted | salsa derives serde (as the raw id) only when the struct is `persist`; so a value type holding an id forces that struct to be persisted too. This is what makes opting in transitive |
| `la_arena::Arena<T>`, `Idx<T>`, `ArenaMap` | hir-def, hir-ty | easy | needs serde impls in `lib/la-arena` (in tree) |
| `Span`, `SyntaxContext`, `SpanMap`, `ExpansionSpanMap` | span, hir-expand | custom | `SyntaxContext` is a salsa interned id (324,895 values on dx); spans are compact but there are a lot of them |
| `tt::TopSubtree` | tt | custom | flat buffer of tokens with spans and symbols |
| `Parse<T>` (rowan green tree) | syntax | recompute | done as events + text; loading is as slow as parsing (section 4) |
| `AstIdMap`, `AstPtr`, source maps (`BodySourceMap`, `*::with_source_map_`) | span, hir-def | recompute | pointers into trees; only needed for diagnostics and IDE features, and derived cheaply from a parse |
| `ItemTree` | hir-def | custom | arenas of items, `ModPath`, `Symbol`, `AstId`, attrs. Mechanical, a few days |
| `DefMap`, `ItemScope`, `ModuleScope` | hir-def | custom | large graph of ids (every item id must be persisted first), macro call ids, `ProcMacros` |
| `ExpressionStore`, `Body`, signatures, `TypeRef`, `Path` | hir-def | custom | arenas plus interned `TypeRef`s and paths |
| `Ty`, `GenericArgs`, `Clauses`, `TraitRef`, `Const`, regions | hir-ty next-solver | custom, hard | pointer-interned in a process-global interner (not salsa). Needs a structural writer with back-references and re-interning on read, for every kind in `rustc_type_ir` that rust-analyzer instantiates |
| `InferenceResult`, `MirBody`, `Layout`, const-eval results | hir-ty | custom, hard | all contain the above; `Layout` is `rustc_abi` data without serde |
| `ProcMacros`, proc-macro expanders | hir-expand | recompute | holds `Arc<dyn Expander>` tied to a running proc-macro server. It is an input the def maps read, so it cannot be skipped: it needs a custom loader that reconnects, or setting it after load invalidates every def map |
| `CompiledFunction` | hir-ty codegen | already done | the per-function store already persists it by content |

"Easy" is about 20 types and was done here in a day for the base-db slice. "Custom" at the
item-tree/def-map level is a few hundred types reachable from `DefMap` and `ItemTree`. "Hard" is
the type system: there is no way to derive it, and it is where most of the 3.5 million memos are.

## 3. The prototype

What is on the branch:

- `Cargo.toml`: salsa `persistence` feature; serde for `triomphe`, `semver`; serde added to
  `edition`, `intern`, `cfg`, `span`, `syntax`, `vfs`, `base-db`, and `postcard` in `ide-db`.
- `persist` on the eight base-db inputs (`FileText`, `FileSourceRootInput`, `SourceRootInput`,
  `LibraryRoots`, `LocalRoots`, `AllCrates`, `Crate`, `IncludedFile`), on `InternedSourceRootId`,
  `EditionedFileId`, and on two queries: `source_root_crates` and `EditionedFileId::parse`.
- `crates/syntax/src/saved.rs`: a `Parse` as enter/token/leave events plus the token text.
- `crates/base-db/src/snapshot.rs`: the tables base-db keeps beside salsa (`Files`, `CratesMap`).
- `crates/ide-db/src/snapshot.rs`: `RootDatabase::save` / `RootDatabase::load` (postcard).
- `Vfs::with_paths` / `Vfs::paths`, so file ids are the same after a restart.
- Setters in `base-db` and `load-cargo` compare before they set.
- `crates/rustd/src/main.rs`: `RUSTD_SNAPSHOT=<file>` loads the file at start if it exists and
  writes it after the first build if it does not. `RUSTD_TRACE=1` prints how often each query
  ran during the first build (`cold ran:`). `RUSTD_DB_STATS=1` prints the ingredient table.
- `crates/ide-db/tests/snapshot.rs`: saves a database, loads it, asks for the parse and asserts
  that no query executed and the tree and errors are equal (`cargo test -p ide-db --test
  snapshot`, passes). That test is in one process; the runs in section 4 are separate processes.

What it shows: save in one process, load in another, the loader reconciles without invalidating,
and `EditionedFileId::parse` answers from the snapshot. On the small workspace `parse_` ran
1,018 times in the saving process and does not appear at all in the `cold ran:` line of the
loading process, with every other query count identical.

What it does not do: item trees and def maps are not persisted. I stopped below them on purpose
once the parse numbers were in, because the next level needs `ProcMacros`, `SyntaxContext`,
`MacroCallId` and every item id to be persistable first, which is weeks, not a slice.

## 4. Measurements

Setup. `~/work/dx` is a worktree of dioxus at 847fbd08d, built with cargo by a `difftest.Fixture`
script, which also writes `work/symbols.txt`. `~/work/small` is the `stress.py` fixture (crates
`sbase`, `smid`, `sapp`, plus the sysroot: 23 crates, 1,018 files parsed). All runs go through

```
run.sh BIN PROJECT WORK CRATES LOG:
  RUSTUP_TOOLCHAIN=$(cat harness/toolchain) RUSTD_STATS=1 \
  /usr/bin/time -v BIN PROJECT --crate CRATES... --out WORK/out.o $(cat WORK/symbols.txt)
```

Binaries: `rustd-base` is `rustd-spike` unchanged; `rustd-p0` persists inputs only; `rustd-p1`
is the branch as committed (inputs + parse). All `cargo build -p rustd --release`.

### dx, cold, no snapshot (`rustd-base`)

`./run.sh ./rustd-base dx/packages/cli dx/work dx LOG`, with `RUSTD_CACHE` unset or set:

| run | load workspace | resolve (710 crates) | first build | wall | peak RSS |
|---|---:|---:|---:|---:|---:|
| first ever, cold file cache | 44.4 s | 11.1 s | 37.3 s | 95.6 s | 9.02 GB |
| again, warm file cache | 3.2 s | 10.5 s | 42.0 s | 59.1 s | 9.03 GB |
| `RUSTD_CACHE` populated (915 MB on disk) | 2.6 s | 9.0 s | 18.5 s | 32.4 s | 10.2 GB |

The first build compiles 1,331 functions and 218,306 instances. With the machine-code store
populated, 172,712 of 219,359 are reused and 46,647 are compiled again.

So of a ~55 s warm start today, roughly: 3 s is loading (cargo metadata, reading files), 10 s is
name resolution and impl collection, and ~40 s is the first build, of which the existing store
already removes about half. What is left after the store (18.5 s build + 9 s resolve) is the
most that persisted analysis could remove, and it would have to cover MIR, inference and layout
to get the build part.

### Small workspace

```
./run.sh ./rustd-p1 small/app small/work sapp,smid,sbase small-cold.log
RUSTD_TRACE=1 RUSTD_SNAPSHOT=$PWD/small.snap  ./run.sh ./rustd-p1 ... small-save.log
RUSTD_TRACE=1 RUSTD_SNAPSHOT=$PWD/small.snap  ./run.sh ./rustd-p1 ... small-load.log
RUSTD_TRACE=1 RUSTD_SNAPSHOT=$PWD/small0.snap ./run.sh ./rustd-p0 ... small0-save.log
RUSTD_TRACE=1 RUSTD_SNAPSHOT=$PWD/small0.snap ./run.sh ./rustd-p0 ... small0-load.log
```

| run | snapshot | save | load | load workspace | resolve | first build | wall |
|---|---:|---:|---:|---:|---:|---:|---:|
| p1 cold, no snapshot | | | | 255 ms | 1.9 s | 296 ms | 2.58 s |
| p1 save (inputs + parse) | 99.5 MB | 700 ms + 43 ms write | | 262 ms | 1.7 s | 245 ms | 3.05 s |
| p1 load | 99.5 MB | | 2.3 s + 44 ms read | 2.6 s | 1.3 s | 239 ms | 4.19 s |
| p0 save (inputs only) | 47.0 MB | 55 ms + 42 ms write | | 595 ms | 2.6 s | | 3.78 s |
| p0 load | 47.0 MB | | 34 ms + 25 ms read | 360 ms | 2.4 s | | 3.17 s |

("load workspace" includes the snapshot load.) Loading 1,018 saved parses took about 2.2 s more
than loading inputs alone and saved about 0.4 s of resolve. Parsing them is cheaper than reading
them back. The p0 rows differ from each other only by noise; the query counts in the two logs
are identical.

### dx with snapshots

```
RUSTD_CACHE=~/work/cache-base RUSTD_SNAPSHOT=$PWD/dx0.snap \
    ./run.sh ./rustd-p0 dx/packages/cli dx/work dx dx0-save.log     (then again: dx0-load.log)
RUSTD_DB_STATS=1 RUSTD_TRACE=1 RUSTD_SNAPSHOT=$PWD/dx1.snap \
    ./run.sh ./rustd-p1 dx/packages/cli dx/work dx dx1-save.log     (then again: dx1-load.log)
```

| run | snapshot | save | load | load workspace | resolve | first build | wall | peak RSS |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| p0 save (inputs) | 419 MB | 540 ms + 169 ms | | 3.3 s | 10.0 s | 31.6 s | 48.5 s | 12.2 GB |
| p0 load | 419 MB | | 519 ms + 124 ms | 3.1 s | 9.4 s | 19.9 s | 34.6 s | 10.9 GB |
| p1 save (inputs + parse) | 593 MB | 3.3 s + 271 ms | | 3.3 s | 12.3 s | 43.6 s | 66.7 s | 9.31 GB |
| p1 load | 593 MB | | 8.2 s + 206 ms | 10.9 s | 9.5 s | 39.1 s | 62.7 s | 8.93 GB |

Reading these correctly:

- The p0 pair ran with `RUSTD_CACHE`. The store is keyed per binary, so the save run was the
  first run of that binary and reused nothing, and the load run reused 172,712 functions. The
  drop from 31.6 s to 19.9 s is the machine-code store, not the snapshot. Compare the p0 load
  row with the `rustd-base` cached row instead (32.4 s wall, 9.0 s resolve, 18.5 s build): the
  inputs snapshot changes nothing measurable.
- The p1 pair ran without `RUSTD_CACHE`. The parse snapshot adds 174 MB and 7.7 s of load, and
  the total is the same as cold within noise.
- On dx the parse query still ran after loading: 13,251 executions in the saving process,
  9,539 in the loading one, for 9,812 files. I did not chase this down. The likely cause is
  that `parse` is `lru = 128`: evicted memos have no value and are not saved, so only the trees
  that were alive at save time came back. That is a guess from reading salsa, not something I
  verified.
- zstd -3 takes the 419 MB inputs snapshot to 67 MB in 0.59 s (`zstd -3 -T8 dx0.snap`).

### Process snapshot of the daemon (CRIU)

`~/work/criu.sh`, `rustd-base` unchanged, daemon started as
`setsid rustd-base dx/packages/cli --crate dx --out ... --socket dx/work/rustd.sock <symbols>`,
then `sudo criu dump -t PID -D img`, then `sudo criu restore -D img -d`, then
`rustd-base sync dx/work/rustd.sock`.

| step | result |
|---|---|
| warm-up before the dump | 53.1 s, 8.32 GB RSS |
| `criu dump` | 5.55 s, image 8.0 GB |
| `criu restore -d`, image in page cache | 3.43 s |
| `sync` after restore | `ok unchanged`, RSS 8.30 GB |
| append a function to `packages/cli/src/rustcwrapper.rs`, `sync` | `ok ... functions=0 files=1 ms=4721`; log: 0 of 219,409 compiled, 219,409 reused |
| kill, `echo 3 > /proc/sys/vm/drop_caches`, `criu restore -d` again | 7.47 s |
| `sync` after that restore (file mtime now differs from the image) | `ok ... files=1 ms=4996` |
| `zstd -3 -T8` of the page image | 6.97 s, 1.55 GB; `zstd -d` 8.01 s |

This needed CRIU 4.2.1 from `ppa:criu/ppa`. The 3.16.1 that Ubuntu 22.04 ships dumps fine and
then segfaults on restore on this kernel, even for `sleep`.

The ~4-5 s for a sync after touching one file is not something I compared against an unrestored
daemon in the same session, so I cannot say whether the restore adds to it. The edit log during
that sync also printed `outline: FileId(23408): a listed function is gone` and a scoped-thread
panic message before completing with `ok`; I do not know whether `rustd-spike` does that without
CRIU for the same edit.

## 5. Alternatives

**A. Salsa persistence (what the prototype starts).**
For: it is the only option that gives a fresh process on a different machine real incremental
state, and salsa validates restored memos against restored inputs for free.
Against: everything read by a persisted query must be persisted, so it cannot be done query by
query from the top; it has to be built bottom-up through item trees, def maps, signatures,
bodies, inference, MIR. The value types are a fork-wide change against upstream rust-analyzer,
which does not enable the feature. The snapshot is tied to the exact binary. And the one data
point there is on load speed is bad: postcard + rebuilding green trees ran at about 22 MB/s
(174 MB in 7.7 s), against 800 MB/s for plain inputs. Types will need re-interning on load,
which is closer to the tree case than to the inputs case. Loading 3.5 million memos and their
heap (most of 8 GB) through serde could plausibly take as long as the 10 s of resolve it saves.
That last sentence is an estimate; nothing here measures it.

**B. Selected expensive queries, keyed by content hash, outside salsa.**
This is what the machine-code store already is, and it is the reason a restart costs 32 s
rather than 59 s. Extending it means: find a query whose inputs can be hashed without running
the analysis, and whose output can be serialized without ids. The machine code qualifies because
its key is content. For MIR, inference and layout I do not see such a key: their inputs are
"everything name resolution and trait solving looked at", and computing that set is the
analysis. A per-crate def-map cache keyed by (crate source hash, cfg, dependency hashes) is the
one candidate that has a natural key, and it would attack the 10 s of resolve; it needs the same
`DefMap` serializer as option A but none of salsa's ordering and id constraints, because ids
would be re-created on load. The 46,647 functions that the store does not reuse on a restart
(27 % of instances) are worth looking at before any of this: it may be cheaper to find out why
they miss than to persist anything new.

**C. Process snapshot (CRIU), or a fork server.**
CRIU, measured above: 3.4 to 7.5 s to a warm daemon, no rustd changes, nothing to maintain per
type. Costs: 8 GB image (1.55 GB compressed, and then ~8 s to decompress), root or
`CAP_CHECKPOINT_RESTORE`, Linux only, same binary, same paths, same kernel features, and it
needs a recent CRIU. Files that changed while the daemon was down are picked up by the normal
edit path, as the second restore shows. Not tested: restoring on a different VM, the watcher
mode (`--watch`, inotify), a daemon with live proc-macro server children or open client
connections, the remote store connection. On macOS there is no equivalent.
A fork server (keep one warm process, fork per session) only helps where a warm process already
exists on the machine; it does nothing for a fresh VM and rustd is already that process.
VM-level snapshots (the VM image with the daemon running) are the same idea one level down and
are what this VM platform already does on sleep; if the fresh VM is made from a snapshot taken
with the daemon warm, there is nothing to do in rustd at all.

**D. mmap-able formats (rkyv, or a custom arena layout).**
Would fix load time: no parsing, pages fault in on demand, and 8 GB does not have to be read
up front. But salsa's tables and memos are not laid out for it, and the values are full of
`Arc`, `Box`, hash maps and pointer-interned types. This means rewriting the storage of the
values, not adding derives; it is strictly more work than A and should only be considered for a
single large, flat, read-mostly structure (the machine-code store's index, or a def-map cache).

| | restart, same machine | fresh VM, same image | other machine / new binary | work in rustd |
|---|---|---|---|---|
| today, machine-code store | 32 s | 32 s if the store is shared | 32 s if the store is shared | none |
| A. salsa, inputs only | no gain (measured) | no gain | invalid on new binary | done here |
| A. salsa, through def maps | maybe -5 to -10 s (estimate) | same | invalid on new binary | weeks |
| A. salsa, everything | unknown; load time is the risk | same | invalid on new binary | months |
| B. def-map cache by content | maybe -5 to -10 s (estimate) | same | works | weeks |
| C. CRIU | 3.4 - 7.5 s (measured) | likely the same, untested | does not apply | days |
| D. mmap | n/a without A or B | | | more than A |

## 6. Recommendation and plan

Do not continue with whole-database salsa persistence now. The inputs are trivial and useless on
their own, the first derived level is slower to load than to recompute, and the levels that hold
the time (types, MIR, layout) are the ones that are hardest to serialize and most likely to load
slowly.

Use a process snapshot for the restart and same-image cases, and keep the content-keyed store as
the portable layer.

Stages, with rough costs (one engineer, or the equivalent in agent sessions):

1. CRIU as an opt-in restart path. 2-4 days. A `rustd` subcommand or wrapper that dumps a warm
   daemon and restores it, with the checks that make it safe: same binary, same workspace root,
   same toolchain. Then test the things not tested here: `--watch` mode, proc-macro server
   children, open sockets, restore on a second VM from the same image, behaviour when the
   checkout changed a lot in between. Decide whether the image is compressed (1.55 GB, +8 s) or
   not (8 GB, 3-7 s). Expected result: restart in under 10 s instead of 32-55 s.
2. Look at why 46,647 instances miss the machine-code store on restart. 1-3 days to diagnose.
   This is the largest known piece of the 32 s that is not analysis, and it helps every
   platform.
3. Measure before building anything else: a profile of the 9-10 s resolve and of the 18.5 s
   cached build, split into def maps, impl collection, inference, MIR, layout, monomorphization.
   1 day. This report does not have that split, and stage 4 should not start without it.
4. Only if 1 is not usable (macOS, or VMs that cannot share an image) and 3 says name resolution
   is worth it: a per-crate def-map + item-tree cache keyed by content, outside salsa. 3-5 weeks:
   serializers for `ItemTree`, `DefMap`, `ItemScope`, spans and macro call ids, re-creation of
   ids on load, and a correctness harness comparing loaded against computed maps on dx. Expected
   gain bounded by the 10 s of resolve.
5. Full salsa persistence through hir-ty: not planned. 2-4 months and a permanent divergence
   from upstream in every hir crate, for an unknown load time. Revisit if salsa grows a
   zero-copy format or upstream rust-analyzer turns the feature on.

What to keep from this branch regardless: the `RUSTD_TRACE` execution counts and
`RUSTD_DB_STATS` ingredient table are useful for stage 3. The serde derives, `persist`
attributes, snapshot modules and `RUSTD_SNAPSHOT` are only worth keeping if stage 4 or 5 happens.

## 7. Unverified or open

- Every timing is a single run. Noise on the dx build is several seconds.
- Why `parse` still ran 9,539 times after loading on dx (LRU eviction is a guess).
- Whether the `persistence` feature itself costs time or memory when nothing is saved; not
  visible in single runs, not measured properly.
- The split of resolve and build time by query.
- CRIU: only the socket daemon, same VM, same boot. The post-restore sync times and the
  `a listed function is gone` message were not compared against an unrestored daemon.
- The estimates in the tables of sections 5 and 6 marked as estimates.
- The inventory script counts attributes by regex.
- The snapshot header (executable size and mtime) is not a real compatibility check, and the
  snapshot format is whatever postcard writes for this build.
- No rustd harness (`stress.py` etc.) was run against the prototype binary.
