# rustd: project plan and harness

rustd is a build daemon that keeps a Rust workspace loaded in rust-analyzer's database and
compiles edited functions straight from RA's MIR with Cranelift. rustc still builds everything
once and stays the reference compiler. This file says what we are aiming for, how we know whether
we are getting there, and how the work is split up.

Current numbers live in [harness/SCOREBOARD.md](harness/SCOREBOARD.md). They are generated; this
file should not repeat them.

## 1. Goal

Make the edit-to-running-code loop on a real Rust project take tens of milliseconds instead of
seconds, without ever running code that behaves differently from what rustc would have built.

Not goals, at least for now:

- Replacing rustc. Release builds, clean builds and CI stay on rustc.
- Diagnostics. Errors and borrow checking come from `cargo check` running beside the daemon.
- Optimised code. The fast tier is debug-quality output.
- Producing `.rlib`/`.rmeta` for other crates to depend on.

## 2. The two rules

Everything else follows from these.

**Rule 1: rustc is the authority.** A function compiled by rustd must be observably the same as
rustc's version. When they differ, rustd is wrong, and that is a release-blocking bug, not a
coverage gap.

**Rule 2: when unsure, fall back.** The backend only compiles what it has explicit support for.
Anything else (an unknown MIR construct, a type RA could not fully infer, a layout it cannot
classify) is an `unsupported` error for that function, and the function stays with rustc. A
fallback costs time. A wrong guess costs trust. There is no "probably fine" path.

The harness exists to enforce rule 1 and to measure how often rule 2 fires.

## 3. Metrics

Four numbers, in priority order. A change that improves a lower one while hurting a higher one
does not land.

| # | Metric | What it measures | How |
|---|---|---|---|
| 1 | Mismatches | Targets whose behaviour differs from rustc. Must be 0. | `harness/difftest.py` |
| 2 | Coverage | Share of functions rustd compiles, per crate, and the reasons for the rest | `harness/scoreboard.py` |
| 3 | Edit latency | Time from file save to runnable code, split into re-analysis, codegen, link/patch | `harness/bench.py` (to build) |
| 4 | Footprint | Daemon RSS, cold load time | same |

Coverage is counted over functions rustc emitted a symbol for (the ones rustd can replace).
Generic and `#[inline]` functions have no symbol of their own; they are compiled as instances
when something reachable calls them and are reported separately.

Coverage counts functions, not execution. A crate at 40% can still spend 100% of its edit loop in
fallback if the edited function is in the other 60%. Once the daemon exists, the number that
matters is the share of real edits served by the fast tier, and the bench should report that.

## 4. The harness

### What exists

- `harness/difftest.py <workspace> --bin <pkg> --crate <name>...`
  Builds the binary with cargo, compiles the named crates with rustd, links rustd's objects in
  front of rustc's rlibs, runs both binaries and compares stdout, stderr and exit status.
  With `--bisect` it also links each rustd function in on its own and names the ones that change
  behaviour, with the first differing output line.
- `harness/corpus/`: a lib crate with one module per backend area (scalars, control flow,
  aggregates, enums, slices, constants, generics, closures, trait objects, heap) and a runner that
  calls every function and prints the result. This is the unit-test suite for the backend.
- `harness/scoreboard.py`: runs every target in `targets.json`, writes `scoreboard.json` and
  `SCOREBOARD.md`, fails on any mismatch or on coverage going down. `--update` accepts new numbers.
- `rustd --report <tsv>` (per-function outcome), `--deny <file>` (leave these functions to rustc),
  `--isolate <dir>` (one object per function).

It has already paid for itself: the first run found that `hex::val` was miscompiled, and the
corpus then showed six functions with the same cause (16-byte aggregates were being returned in
registers; rustc returns them through a pointer). All fixed, all targets match now.

### What to build next

1. **Panic and abort fixtures.** The corpus only has functions that return. Overflow, bounds
   checks, `unwrap` on `None` and explicit `panic!` need their own binaries, compared on exit
   status and panic message.
2. **Crate test suites as differential tests.** For a real crate, build its own `cargo test`
   binary with rustd objects linked in and require the same pass/fail set. This exercises far
   more paths than a hand-written `main` and costs nothing to write. This is the main correctness
   gate for real code.
3. **`bench.py`.** Start the daemon on a target, apply a fixed list of one-line edits, record
   re-analysis, codegen, link and total per edit, plus RSS. Same edits against `cargo build` for
   the baseline. Output goes on the scoreboard.
4. **Crash containment.** A Cranelift panic currently takes down the whole run (this happened
   with i128 arguments). Each function's codegen should be caught and reported as a skip with
   reason `backend panic`, which the scoreboard treats as a bug count.
5. **Random programs.** Rustlantis generates MIR-shaped Rust programs for exactly this kind of
   differential testing and has found bugs in rustc's own backends. Worth wiring in once the
   corpus passes; not before.
6. **CI.** `scoreboard.py` on every push to the fork. It runs in under a minute today.

### Target ladder

Each rung is a set of entries in `targets.json`. We move up when the rung below is at zero
mismatches and the coverage bar is met. Bars are proposals.

| Rung | Targets | Bar |
|---|---|---|
| 0 | corpus, toy | 100% of corpus functions, 0 mismatches |
| 1 | small deps: semver, itoa, hex, httparse, heck | 90% of functions, their test suites pass |
| 2 | mid-size sync crates: regex-syntax, serde_json, toml, syn | 80%, test suites pass |
| 3 | a Dioxus example app (workspace crate only, deps from rustc) | 80% of sync functions, app runs, edit latency measured |
| 4 | `dx` itself | async coverage counted, edit latency under a stated budget |

## 5. Workstreams

Each one has an owner session, a list of scoreboard reasons it is responsible for removing, and
corpus modules it must keep green. They touch different parts of `codegen.rs` (which should be
split into a directory along these lines before parallel work starts).

| | Workstream | Removes these scoreboard reasons | Depends on |
|---|---|---|---|
| A | Harness (section 4 list) | n/a | nothing; goes first |
| B | Calls and ABI: odd-sized aggregates, fn pointers, indirect calls, closures, floats | `odd-sized aggregate`, `indirect call`, `ReifyFnPointer cast`, `closure aggregate` | A |
| C | Trait objects: vtable layout matching rustc, unsizing, dyn calls | `call through a vtable`, `unsizing` | B |
| D | Drop glue and unwinding: drop shims, cleanup blocks, landing pads | `drop glue` | B |
| E | Core intrinsics and builtin derives | `const_eval_select`, `builtin derive method`, other intrinsics | A |
| F | Constants and statics: thread locals, statics with relocations, promoted consts | `static without a rustc symbol` | A |
| G | MIR gaps in RA itself: range-pattern consts, `offset_of!`, and later async | `mir: NotSupported` | A |
| H | Daemon: file watching, cargo integration, private artifact dir, fallback orchestration | n/a (latency) | rung 1 |
| I | Output: subsecond patches instead of relinking | n/a (latency) | H |
| J | Async: coroutine state-machine transform on RA MIR | async bodies | D, G |
| K | CAS: content-addressed store for rustc artifacts and rustd objects, remote backend | n/a (clean builds) | H |

B through G are independent of each other apart from the arrows and can run as parallel sessions
once A is in place. H and I are the path to something usable in `dx`. J is the largest single
item and the one most likely to need changes inside RA rather than beside it.

How a change lands, for every workstream:

1. Add or extend a corpus function that needs the feature. It shows up as skipped.
2. Implement. The function moves to compiled.
3. `scoreboard.py` passes: no mismatch anywhere, no coverage drop.
4. Commit code and the updated scoreboard together, so the history of the scoreboard is the
   history of the project.

## 6. Integration points

**rustc.** We link against its rlibs, so we must agree with it on symbol names (v0 mangling),
type layout, the unstable Rust call ABI, vtable layout and panic entry points. All of these can
change between rustc versions. rustd is pinned to one toolchain at a time, the pin is recorded
in the repo, and a toolchain bump is a scoreboard run like any other change. Layout agreement
deserves its own check: compare RA's computed layout for every type in a target against
`rustc -Zprint-type-sizes` and count disagreements. Any disagreement is a silent miscompile
waiting to happen, and today we do not measure it.

**cargo.** The daemon needs the unit graph, the rustc command line for each crate and the final
link line. Today that is scraped from `cargo rustc -- --print link-args`. The cleaner source is
`cargo build --unit-graph` / `--build-plan` or a `RUSTC_WORKSPACE_WRAPPER` that records
invocations. The daemon must keep its own copy of the artifacts it links against: sharing
`target/` with a concurrent `cargo build` already broke one link.

**rust-analyzer.** We use `hir-ty`'s MIR, monomorphisation, layout and const-eval queries, and
`load-cargo` for workspace loading. The backend lives in `crates/hir-ty/src/mir/codegen.rs`
because it needs crate-private access; the goal is to shrink that to a small set of `pub`
additions so the backend can sit in its own crate.

**Cranelift.** Used as a library through `cranelift-object`. No changes needed so far.

**subsecond / dx.** subsecond already loads a patch object into a running process through a jump
table. rustd's object is the same kind of thing with a different producer, so the integration is
`dx serve` asking the daemon for a patch instead of running rustc. Fallback is the existing path.

**CAS.** Keys are (toolchain, crate source hash, flags, dependency keys) for rustc artifacts and
(function MIR hash, instance args, toolchain) for rustd objects. Independent of the fast tier
and useful on its own for clean builds.

## 7. Tracking upstream rust-analyzer

The fork is RA plus two things: the `rustd` crate and the codegen module. Policy:

- Rebase onto upstream weekly. The scoreboard is the acceptance test for the rebase.
- Keep changes to existing RA files minimal and mechanical (visibility, a module declaration).
  Count them; that number is the rebase cost and should go down.
- MIR fixes that are simply correct (workstream G) are candidates for upstreaming by a human.
  RA's AI policy does not allow us to open PRs or issues there ourselves.

## 8. Known unknowns

- Whether RA's inference is right often enough. Lowering succeeds for ~97% of `dx` bodies, but
  "lowered" is not "lowered correctly". Crate test suites (4.2) are how we find out.
- Warm re-analysis time on a large workspace. 3 ms on the toy says nothing about `dx`.
- RA's memory use: 8 GB for `dx`. Possibly acceptable for a daemon, possibly not.
- Unwinding. RA's MIR has no cleanup edges at all; adding them is real work inside RA's lowering.
- Debug info. None today.
