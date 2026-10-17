# Contributing

## AI use

Most of this code was written with AI assistance under human direction. The same is true
of the harnesses, the reports under `docs/reports/` and these documents. The reports are
written in the first person by the session that did the work; "I ran" in a report means
that session ran it.

What makes that workable is the testing method. Every change is judged by comparing rustd's
output with rustc's on real programs, and a change that makes a harness differ does not
land. If you contribute with AI assistance, the expectation is the same as without: you
understand the change, you ran the gate, and you say what you ran and what you did not.

rust-analyzer has its own AI policy for contributions to rust-analyzer. It applies to what
you send them, including fixes you find here and want to upstream.

## Layout of the tree

    crates/rustd/             the daemon, the cargo drop-in, the splice, the remote
    crates/rustd/src/bin/     cargo-rustd
    crates/hir-ty-codegen/    the code generator: MIR to Cranelift IR, the producers, the store
    crates/rustd-wasm/        experiment: the compiler running in a web page (not in the workspace)
    harness/                  the test harnesses and their fixtures
    demo/                     small projects the harnesses and the scoreboard build
    docs/                     these documents
    docs/reports/             working reports, one per piece of work, as written at the time
    vendor/rust-analyzer/     rust-analyzer, a git submodule at the pinned commit
    vendor/rust-analyzer.rev  that commit
    vendor/patches/           rustd's changes to rust-analyzer, one patch per crate
    vendor/rustc_abi/         rustc's layout crate, copied from the 1.99.0 tag
    vendor/apply.py           applies the patches to the submodule
    vendor/source.rev         the commit of the development branch this tree was generated from

The published tree is generated. Development happens on a branch that is rust-analyzer's
tree with rustd's changes made in place, and `publish/split.py` on that branch writes the
layout above: rustd's own files are copied, every rust-analyzer file rustd changed becomes
part of a patch, and `publish/overlay/` is copied over the result. If you are working on
that branch, edit files where they are and edit these documents under `publish/overlay/`.

`crates/hir-ty-codegen` is not a crate. It is the module `hir_ty::mir::codegen`, kept
outside the submodule because all of it is rustd's. To build, its files have to be at
`vendor/rust-analyzer/crates/hir-ty/src/mir/codegen.rs` and `.../mir/codegen/`, and the
patch to `hir-ty` declares the module.

## The rust-analyzer pin and the patches

`vendor/rust-analyzer` is pinned to the commit rustd forked from, recorded in
`vendor/rust-analyzer.rev`. rustd changes about 160 files of rust-analyzer in 22 crates.
Those changes are in `vendor/patches/<crate>.patch`, one file per crate under `crates/`,
and `workspace.patch` for files outside them. Each is a plain `git diff` against the pin.

After cloning:

    git submodule update --init
    python3 vendor/apply.py
    cargo +1.99.0 build -p rustd --release

<!-- APPLY-PY: `vendor/apply.py` was not in the tree at the commit these documents were
     written against (f191c6e42a). The steps in this section were checked by doing by hand
     what it has to do: `git apply` each patch in the submodule and copy
     `crates/hir-ty-codegen/*` to `vendor/rust-analyzer/crates/hir-ty/src/mir/`. Adjust the
     wording below to what the script actually does, in particular whether it copies or
     links the code generator and whether it can be run twice. -->

`apply.py` applies every patch to the submodule's working tree and puts the code generator
where `hir-ty` expects it. The submodule then shows as modified in `git status`. That is
expected; do not commit the submodule pointer.

### Changing a patched rust-analyzer file

Edit the file in `vendor/rust-analyzer` after the patches are applied, build and test, then
regenerate the patch of the crate you touched from the submodule's working tree:

    cd vendor/rust-analyzer
    git add -A crates/<crate> ':!crates/hir-ty/src/mir/codegen.rs' ':!crates/hir-ty/src/mir/codegen'
    git diff --cached --no-renames --full-index --binary HEAD -- crates/<crate> \
        > ../patches/<crate>.patch
    git reset -q

The `git add` is there so that files the patch creates are in the diff; the two exclusions
keep the code generator, which lives in `crates/hir-ty-codegen`, out of the `hir-ty` patch.
Run on an unchanged tree, this reproduces the existing patch byte for byte, so the diff of
`vendor/patches/` in your commit is your change and nothing else. Commit the patch file.

Keep the patches small. A change to rust-analyzer should be something rust-analyzer could
take: a bug fix in MIR lowering or inference, a missing piece of the language, an accessor
rustd needs. Code that is only rustd's belongs in `crates/hir-ty-codegen` or
`crates/rustd`. Fixes that are simply rust-analyzer bugs should be sent upstream, under
their rules.

A change to the code generator is made in `crates/hir-ty-codegen`. If `apply.py` copied
those files into the submodule, copy your edit back or apply again before building.

### Moving the pin

Moving the pin means rebasing the patch series onto a newer rust-analyzer.

1. In `vendor/rust-analyzer`, with the patches applied and committed on a local branch,
   rebase that branch onto the new upstream commit and resolve conflicts.
2. Build, and run the whole gate under both backends ([harness.md](harness.md)). A newer
   rust-analyzer changes inference, MIR lowering and layout queries, any of which can
   change generated code. The corpus with `--layouts`, `breakers.py` and
   `fuzz/fuzz.py smoke` are the ones that notice first.
3. Regenerate every patch against the new commit, write the new commit to
   `vendor/rust-analyzer.rev`, and update the submodule pointer.
4. Update `Cargo.toml` and `Cargo.lock` at the root for dependencies rust-analyzer changed.

Nobody has moved the pin yet, so this procedure is the intended one and has not been
exercised.

The toolchain is a separate pin. rustd is built for Rust 1.99.0 and `vendor/rustc_abi` is
rustc's crate from that tag. Moving to another Rust version means taking the new
`rustc_abi`, rechecking everything that mirrors rustc (the Rust ABI in the code generator,
`vtable.rs`, `type_id.rs`, symbol names in `symbols.rs`), and running rustc's own test
suites through `uitest.py` and `libsuite.py` again.

## Rules for code

1. **rustc is the reference.** If rustd's program behaves differently from rustc's, rustd
   is wrong, even where rustc's behaviour looks like an accident.
2. **Never guess.** Input the code generator has no explicit lowering for is refused with a
   reason (`CodegenError`), not lowered on a best-effort basis. A refusal is visible and
   counted. A wrong guess is a miscompile nobody sees.
3. **A refused function is a bug**, to be listed and fixed, not a normal path to design
   around.
4. **General fixes.** A fix that tests for one crate, one function name or one test is not
   a fix. Find what the case is an instance of.

Style follows rust-analyzer's guide
(`vendor/rust-analyzer/docs/book/src/contributing/style.md`): `cargo fmt`, no new
dependencies without a reason, small focused changes. Comments say what the code is for and
why, not what was changed. Messages printed to the user say what happened and what rustd
did about it in one line.

## What a change needs

- A reduced case that fails before and passes after: a corpus function for a code
  generator feature, a breaker for a bug found in real code, a step in the relevant edit
  harness for a daemon bug. [harness.md](harness.md) says how to add each.
- The gate, under Cranelift and under LLVM, on the platform you have. Say which platforms
  you did not run. CI covers Linux x86-64, Linux AArch64, Windows and macOS.
- `cargo fmt --all -- --check` and
  `cargo test --release -p hir-ty -p hir-def -p parser -p syntax -p rustd`.
- For anything that changes numbers in [results.md](results.md): the command, the commit
  and the machine, so the entry can be replaced and not just appended to.

When you report what you did, separate what you ran from what you inferred. The reports
under `docs/reports/` do this and it is the reason they can be used.

## Licence

rustd is MIT or Apache-2.0, like rust-analyzer. Contributions are accepted under the same
terms.
