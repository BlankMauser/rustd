# macOS arm64: debug info, the build model and the store

Branch `rustd-macos-prod`, cut from `rustd-macos-abi2`, with `rustd-collate` merged in several
times, last at f191c6e42a. Everything under "Measured" ran
natively on an Apple Silicon Mac (`uname -m` is
`arm64`, macOS 26.5.2, stable 1.99.0-aarch64-apple-darwin, LLVM 20.1.8 from Homebrew). The
x86_64-apple-darwin attempt ran under Rosetta 2 and is described separately. Nothing here
ran on Linux locally.

## Debug info on Mach-O

### What changed

- `codegen/debuginfo.rs` used to stop unless the object was ELF. It now also writes the
  DWARF sections into a Mach-O object, under `__DWARF` with Mach-O's names
  (`__debug_info`, `__debug_line` and so on). The frame register is x29 on AArch64.
- The readers of DWARF in a Mach-O object (`dsymutil`, lldb through the debug map) do not
  apply relocations: they expect an address in a debug section to already be the address
  the code has in the object, and move it by the debug map. `DebugContext::settle_macho`
  runs after the object is written and writes those addresses in, where the ELF path
  leaves an offset for the relocation to add to.
- LLVM: the marks that give a function its lines were only made for ELF; they are made for
  Mach-O too, and `llvm/emitted.rs` finds the line table by the name the object format
  gives it.
- The linked binary needs nothing from rustd: `ld` writes an `N_OSO` stab for every object
  with DWARF, and lldb, `atos` and `dsymutil` follow it to the object. The harness checks
  that every object of rustd's is in that map and that its time there is the file's.
- The splice. A patch object is not something `ld` ever saw, so the spliced binary's debug
  map did not know about it and a panic in a spliced function had no frames. `macho::splice`
  now adds the patch object to the map (`N_SO`, `N_OSO` with the path and the modification
  time of the patch, and `N_BNSYM`/`N_FUN`/`N_ENSYM` for each function in it), which grows
  the symbol and string tables in the part of `__LINKEDIT` the splice already rewrites.
  The `__RUSTD_TEXT` segment the splice adds now has a section header, so that these
  entries have a section to name. The signature is made again after, as before.
- Stale or right: the entries of a function that an edit replaced stay in the map and
  still describe the old code at the old address, which is still there and no longer
  called. The patch object is written again at every edit and the splice that follows
  stamps its new time, so the map and the object agree whenever the splice ran. If someone
  changes the object behind the binary's back, lldb and `dsymutil` compare the time in the
  map with the file and say so instead of using it; the harness has a `STALE` verdict for
  that and never got it.
- `unwind.rs`: on Mach-O the personality used to be declared as an import with a signature
  made up on the spot, before it was known whether the crate defines `rust_eh_personality`
  itself. A `no_std` program that does define it then failed with "incompatible with
  previous declaration". The relocation is now written against the function once it is
  known. This is the one change in this round outside debug info that touches codegen.

### Measured, `harness/debuginfo.py`, both backends

The harness is the Linux one; on Darwin it uses `dwarfdump`, `llvm-dwarfdump`, `lldb`,
`atos` and `dsymutil`, and reads the debug map where on ELF it reads the sections. lldb
needs `DevToolsSecurity -enable` and, in a session without a window server, to run as root
(`DEBUGINFO_LLDB="sudo -n lldb"`).

Cranelift, last line `all right (cranelift)`:

- Frames of each panic, against a cargo build: corpus 84 panics and 444 frames the same;
  stress 1 and 3; rung2 1 and 3.
- `llvm-dwarfdump --verify`: no errors on the corpus, stress, rung2, the locals program
  and every one of the six spliced binaries.
- Debug map and `dsymutil`: every object of rustd's is in the map with its time, and
  `dsymutil` makes a dSYM from it without a warning.
- `__eh_frame` against the ranges: 11758 functions in the corpus, none without an entry.
- lldb: a breakpoint by file and line stops in the right function three frames up, and
  stepping goes through the lines of the source, on the corpus, stress, rung2 and the last
  spliced binary. Locals: 9 of 9 printed.
- `atos` through the debug map: 10692 of the corpus's 11758 functions start at a line (the
  rest are glue with no line of their own, as on Linux).
- Splice: six edits in a row, each spliced, each with the same frames as cargo.
- Cost on the corpus: 3.95 s to compile and link without debug info, 4.59 s with it
  (+16.2%); the object grows 7.1%.

LLVM `-O2` fast emit: see the table below for the last line. The same checks run.

Not checked: stripped binaries, and binaries whose symbol table has shapes the corpus and
the harness programs do not have. The splice still declines chained fixups, so with a
deployment target of macOS 12 or later every edit is a full link, debug info included.

## The harnesses that had not run on a Mac

What had to change for them:

- `buildmodel.py`: the socket of a daemon was under a temporary directory whose path is
  longer than macOS lets a socket's be; they are under `/tmp` now. `DYLD_LIBRARY_PATH`
  where Linux has `LD_LIBRARY_PATH`, `shasum -a 256` for `sha256sum`, `sed -i.bak` for GNU
  `sed -i`. A case can now say which hosts it is for (`hosts`), and one that is not for
  this host is reported as `skipped` with the reason: the Linux cross cases
  (`hosts: "-linux-"`) and `rustflags_target_cpu`, which names an x86 CPU.
- `storefull.py`: the full disk is an HFS+ image of a few megabytes made with
  `hdiutil create` and attached with `hdiutil attach`; Linux keeps its tmpfs.
- `store.rs`: when the lock file could not be created the error reported was the one of
  opening it afterwards (no such file), which hid that the disk was full. It reports the
  first one. This is common code; on Linux the case that shows it already passed.
- `keys.py`, `storekills.py`, `storecorrupts.py` and `storeshared.py` ran as they were.

### `fsync` and `F_FULLFSYNC`

The store syncs with `File::sync_all` everywhere (a sealed pack before its rename, the
directory after it). On macOS the standard library implements `sync_all` and `sync_data`
with `fcntl(F_FULLFSYNC)`, not `fsync`: I checked the 1.99.0 binary, where `sync_all`
calls `fcntl` with command 0x33. So the store already asks the drive to write its cache
out, and the decision is to keep it that way and not call `fsync` directly: a pack is
sealed once per build at most, and a store that loses a sealed pack to a power cut is
caught by the hash and rebuilt, but a rename that lands before the data would cost a
whole pack. It is written down above `sync_dir` in `store.rs`.

Measured on this machine's APFS volume, 1 MiB written and then synced: `fsync` 0.52 ms,
`fcntl(F_FULLFSYNC)` 1.21 ms, `File::sync_all` 0.75 ms. This is a virtual machine, so the
numbers say little about a real drive, where `F_FULLFSYNC` is known to cost far more.
`storekills.py` kills the daemon, not the power; nothing here tested a loss of power.

## The gate, measured

Native arm64, on the merge of `rustd-collate` 8f21942505 into this branch, the binary built
with `--features llvm`. Each column is one whole run, one harness after another, nothing
else running beside it. The head also has f191c6e42a merged, which adds two files under
`publish/` and no code, so the gate was not run again for it. The merge of ce52276cf5
before it passed the same gate with the same counts, but for `breakers.py` at 43 of 43.

| harness | Cranelift | LLVM `-O2` fast emit |
|---|---|---|
| `difftest.py corpus --layouts` | MATCH, 828 of 828 from rustd; layouts 0 of 914 differ | MATCH, 828 of 828; 0 of 914 |
| `programs.py` | 22 of 22 | 22 of 22 |
| `stress.py` | 99 of 99, 0 restarts | 99 of 99, 0 restarts |
| `breakers.py` | 58 of 58, 7 open, 0 skipped | 58 of 58, 7 open, 0 skipped |
| `unwinds.py` | 8 of 8 | 8 of 8 |
| `splices.py` | 71 of 71 | 71 of 71 |
| `asmedits.py` / `includes.py` | 10 of 10, 9 of 9 | 10 of 10, 9 of 9 |
| `worktrees.py` plain / `--join --together` / `--overlay` | 62 of 62, 62 of 62, 71 of 71 | 62 of 62, 62 of 62, 71 of 71 |
| `keys.py` | 46 of 46, 0 open | 46 of 46, 0 open |
| `backends.py corpus --levels 2f` | not run | LLVM compiled 11893 of 11893 functions, none left to Cranelift; prints the same as cargo |
| `debuginfo.py` | all right | all right |
| `buildmodel.py` | 188 of 188 as expected, 0 wrong | 188 of 188, 0 wrong |
| `storekills.py --kills 100` | pass | pass |
| `storecorrupts.py --rounds 4` | pass | pass |
| `storefull.py` | pass | pass |
| `storeshared.py` | pass | pass |
| `remote.py` | 178 of 178 | 178 of 178 |
| `cargo_rustd.py` | PASS | PASS |

`buildmodel.py` runs its cases under both backends itself, so each column ran all of it.
It does not run `cross_musl` and `rustflags_target_cpu` (cases of x86-64 hosts) and skips
`cross_aarch64` (needs a Linux cross toolchain), each under both backends.

`remote.py` runs against MinIO and SeaweedFS from Homebrew. `harness/s3fake.py` hands out
each port once: it gave the same port to two services before, and SeaweedFS failed to
start on it.

`breakers.py` says 7 open in its total, and its log names five cases as open (`assert_message`, `unreachable_arm_move`,
`raw_write_then_match`, `future_unwind_flags`, `fragment_edition_through_macro`), known wrong and not counted against the run. They
came with the merges; I did not check whether the same five are open on Linux.

### `cargo rustd` on Mach-O

What it needed, all behind the object format:

- `cargo/plan.rs` reads the link line of Apple's linker: it is Mach-O when it has `-arch`,
  the `env -u` in front of it is kept, and the line gets `-Wl,-headerpad,0x200` for the
  splice. The copies of rustc's objects with weak definitions are made when the link
  command is built, not before, so that cargo's executable is still recognised as the one
  the unit wrote.
- cargo copies an executable out of `deps/` on macOS where it hard-links it on Linux.
  `Record::wrote` takes an artifact for a unit's when it is the same file or has the same
  bytes. It reads both files whole to compare them, after comparing lengths; I did not
  measure what that costs on a large binary.
- `socket.rs`: macOS refuses `set_read_timeout` on a socket whose peer has gone with
  `EINVAL`, which is now taken for the peer having gone.
- `cargo_rustd.py` makes its projects under `/tmp` on macOS: the path of the daemon's
  socket under `/var/folders/...` is longer than a socket path may be.

### The stack of `just`

On the first run after the merge `cargo_rustd.py` failed under Cranelift on one test of
`just` 1.43.0, `recursion_limit::bugfix`: the binary from rustd overflowed its stack where
cargo's reported the recursion limit. rustd's frames were larger: `parse_expression` took
4912 bytes against 3024 from rustc. Every call that passes an argument by a copy in memory
made a stack slot of its own for the copy. Now the calls of a function take turns with
those slots (`ArgumentCopies` in `codegen.rs`): a copy is free again once its call is
made, since the callee owns it and nothing of it is read afterwards. `parse_expression` is
4064 bytes with that, and the test passes.

The margin is thin, and this is measured: with `ulimit -s`, that input needs between 6 and
7 MiB of stack in rustd's binary and between 4 and 5 MiB in rustc's, and the main thread
has 8. The frames are still about a third larger than rustc's (`parse_expression`: 4064
bytes against 3024). Part of the reason is measured: the MIR rustd compiles for
`parse_expression` has 63 locals where rustc's has 41 (9 `Expression` against 5, 8
`Result<Infallible, CompileError>` against 3, 6 `Box<Expression>` against 2), most of the
extra ones being copies of a value made just before a call, and every local gets a stack
slot of its own because the code generator ignores `StorageLive` and `StorageDead`. That
the extra locals add up to the whole difference is inferred, not measured. Sharing slots
between locals that are never live together would close it, but nothing has ever checked
the storage statements of this MIR (the interpreter ignores them too), so it is not a
small change and it is not done.
This is shared code and changes what Cranelift emits on every target.

### Debug settings

They come from the unit graph (`Plan::debug()`), and nothing reads `profile.rs`, which is
gone. One thing was wrong on Mach-O: a patch built without debug info still got a debug
map in the spliced binary. `splice/macho.rs` now writes no map for a patch object without
`__debug_info`. Checked by hand: with debug on the spliced binary has one `N_OSO` for the
patch, and with `[profile.dev] debug = false` it has none.

### Variables and types

Under Cranelift the Mach-O objects describe parameters, locals and their types with the
same code as ELF, from x29. Measured by `debuginfo.py`: 13087 parameters and locals in
11786 functions of the corpus, and lldb prints 9 of 9 values of its `locals` program
(integers, a float, a `&str`, a struct, a tuple struct, an array, a tuple, a reference).
Under LLVM no variable is described, on Mach-O as on ELF: the harness says "LLVM's frames
are not read back yet" on both. So Mach-O has what ELF has; what neither has is variables
under LLVM, and enums, which `DebugType` does not have a case for.

Not run this round: `deps.py`, `scoreboard.py`, `linkbench.py`.

## x86_64-apple-darwin

Time-boxed, under Rosetta 2, in a worktree that is not on this branch. The patch is 45
lines in `target.rs` and `unwind.rs` and is not committed, because the target does not
work.

How far it got, measured:

1. The target is accepted as Mach-O with the System V convention.
2. `ld: __eh_frame unexpected relocation type (1)`, where the last round stopped. x86-64
   Mach-O wants a difference of two symbols as a `X86_64_RELOC_SUBTRACTOR` followed by a
   `X86_64_RELOC_UNSIGNED`, and the personality through the GOT. With that written, `ld`
   failed an assertion (`findRealAtom`): the subtractor must name a symbol, and a section
   has none on Mach-O, so the patch puts one at the start of `__eh_frame`.
3. The corpus then links and runs: 806 of 811 functions from rustd, layouts 0 of 904
   different from rustc, and the first 400 lines of output the same as rustc's.
4. It then dies with SIGSEGV in `async_frames`: on the first poll the state byte of the
   future handed to `block_on` is not a state, and the jump through the table goes
   nowhere. The pointer to the future is right and its contents are not. I did not find
   out why. The same code passes on Linux x86-64 with the same convention, so I suspect
   something particular to Mach-O on x86-64 (a relocation with a wrong addend is my first
   guess) and not the ABI, but that is a guess.

What is missing, in the order it would have to be done:

1. The cause of that crash, and whatever else the remaining lines of the corpus show. A
   second look got no further: lldb never attached to the binary under Rosetta, and after
   the attempts every x86-64 process on this machine hangs at start, a hello world
   included, and cannot be killed. Restarting `oahd` did not help; it needs a reboot.
2. The five variadic definitions the corpus has with named parameters on the stack, which
   are refused on x86-64 everywhere (`rustd-plat-abi2` has that code now).
3. Unwinding through rustd's frames was not reached. The panics before the crash did
   print, so at least the tables link, but no `catch_unwind` case had run yet.
4. LLVM: `llvm/emitted.rs` reads arm64 Mach-O relocations only; the x86-64 ones
   (`X86_64_RELOC_BRANCH`, `SIGNED`, `GOT_LOAD`, `TLV`) are not there. Not tried.
5. TLS on x86-64 Mach-O. Not tried.
6. The splice: `splice/macho.rs` is arm64 only (branch range, page and page-offset
   fixups, stubs). Not tried.
7. Debug info and the harnesses other than the corpus. Not tried.
8. A place to run it: a macOS runner on Intel, or Rosetta on the arm64 one, where a binary
   has to be signed before it runs (the attempt used a wrapper as `RUSTD_RUNNER`).

## CI

The workflow is the one of `rustd-collate` ce52276cf5, which has a macOS job of its own
now. On this branch that job also runs `debuginfo.py` (after `DevToolsSecurity -enable`,
with lldb as root), `buildmodel.py` on the Cranelift leg as the Linux job does, and
`storefull.py`; the collated one left those out as not written for Mach-O. `remote.py`
and `cargo_rustd.py` are not in the macOS job: the first needs MinIO on the runner and the
second downloads and builds seven projects, and I did not try either there. What CI says
of the head is in the final message of the round, not here.

## Shared code to check on Linux

- `store.rs`: which error a lock file that cannot be created reports.
- `splice.rs`: `Baseline::splice` takes the path of the patch and reads it, where it took
  the bytes. ELF, PE and wasm get the same bytes as before.
- `unwind.rs`: `name_personality` takes `&mut self`; the ELF path through it is the old
  one.
- `debuginfo.rs`: the check for the object format, and the names of the sections, go
  through one `match` now.
- `llvm.rs`: each LLVM module is made in a context of its own, which is kept until the
  next one on the thread is made. This came from looking at a timeout in `buildmodel.py`
  under LLVM that I could not reproduce afterwards; it is harmless but I cannot say it
  fixed anything.
- `buildmodel.py`: the `hosts` key and the short socket paths. `hosts` stays beside the
  `on` key of the collated branch: `on` is the architecture, `hosts` the system.
- `codegen.rs`: `ArgumentCopies`, described above. Every target's Cranelift code changes.
- `cargo/units.rs`, `cargo/state.rs`: an artifact is a unit's when it is the same file,
  as before, or a copy of it.
- `cargo/plan.rs`: the ELF link line goes through the parser that now also reads Mach-O.
- `socket.rs`: an `EINVAL` from `set_read_timeout` is not an error.
- `harness/s3fake.py`: a port is handed out once.
- In the merge of ce52276cf5 I took the collated side for every conflict in `codegen.rs`,
  `fragment.rs`, `debuginfo.rs` and `llvm.rs` (`sym` operands, the frame register for
  variables, the LLVM context kept per thread), so the `llvm.rs` item above is gone.

## What is left, in order

1. The splice with chained fixups, so that a binary for a current deployment target is
   spliced at all. Everything about the splice here, debug info included, is for binaries
   with dyld info.
2. x86_64-apple-darwin, from the list above.
3. Debug info of a stripped binary and of a binary linked with `-dead_strip`: not looked at.
4. A test of the store under a real loss of power, on a real drive.
5. rustd's Cranelift frames are a third larger than rustc's, so `just` passes with about
   1 MiB of stack to spare: share stack slots between locals that are not live together.
6. Variables under LLVM and enums in debug info, on every format.
7. The clippy error in `remote.rs`.
