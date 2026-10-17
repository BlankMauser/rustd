# Security review of the store, the remote and the daemon's boundaries

Branch `rustd-security`, on top of `rustd-collate` at 0151c01218. Everything below was
measured on Linux x86-64 with Rust 1.99.0 and LLVM 20. Where something is an argument and
not a measurement it says so.

## Trust model

**The local store is the user's own.** Whatever is in the store directory is machine code
that the next build links and runs, found by a key anyone can compute. So whoever can
write to the directory can make the user's builds run anything. The per-pack SHA-256 and
the per-record CRC catch damage, not malice: an attacker writes a well-formed pack. The
only defence is that nobody else can write there, and rustd now enforces that instead of
assuming it. The store directory is created 0700 and its files 0600. A directory that
belongs to another user, or that group or others can write to, is refused with the reason
and the build goes on without a store. A file inside it with the same problem is not read.
Another process running as the same user is inside the boundary; rustd does not defend
against it and cannot.

**A remote is trusted through signed lists.** Packs and dependency archives on a remote are
named by the SHA-256 of their bytes, so the only thing a writer to the remote can change
without being caught is the list (manifest) that says which hashes belong to a project.
The `rustd-s3` branch signs those lists with Ed25519. A machine that pulls takes a list
only if a key it trusts signed it. Pulling from a remote with no trusted key is refused
unless `RUSTD_REMOTE_UNSIGNED=1`, which is the user saying that everyone who can write to
the remote is trusted as much as the local user. Pushing unsigned is allowed and asks for
nothing.

**The daemon socket is the user's own.** The socket lives in a 0700 directory, is 0600,
and the daemon checks that the peer is the same user. There is no boundary between two
clients of the same user. That code is on `rustd-cargo` and is reviewed, not changed, here.

**What a signer vouches for.** A trusted signer vouches that the code in its packs was
compiled by an honest rustd from the sources the keys name. A compromised signer, or a
signer that builds untrusted code in the same store it pushes from, is outside what
signing can fix.

## Findings, by severity

### High

**H1. Crate identity was 64 bits. Fixed.** `Contents::id` and `Contents::library` kept
eight bytes of a SHA-256 as the id of a crate's content, and that id is the crate part of
every stable function name, which is what store keys are hashed from. On a shared remote
that is a birthday bound of about 2^32 crates for an accidental wrong-code hit, and a
chosen collision is within reach of someone who wants to plant code for a crate they do
not control. The id is now `ContentId`, all 32 bytes, printed in full into the name. The
hashes that feed it were also changed to hash their dependencies' full ids, and the entry
key now length-prefixes each field (`rustd entry 6`) so that no two field lists are the
same bytes. Test: `cached::tests::a_crate_is_named_by_all_of_its_hash_and_a_key_by_all_of_a_name`
builds two ids that agree in every byte the old id kept and checks the names and keys
differ, and that debug, overflow checks and a trailing byte each change the key.

**H2. A store that others can write to was used. Fixed.** Nothing checked who owned the
store or what its mode was, and files were created with the process umask (typically
0644 in a 0755 directory). Now: created 0700/0600; an existing directory that is readable
by others is closed to them; one that is another user's or group/world-writable is refused
by the daemon (`store: not used`, with the reason, and the build succeeds uncached) and by
`rustd cache stats`, `gc` and `verify`. Files are checked with `fstat` on the handle that
is then read, so there is no window between check and use. `rustd cache verify` lists a
pack others can write to as corrupt; a daemon removes it. Tests in `store::tests`:
`the_store_and_its_files_are_the_users_alone`, `a_store_others_could_read_is_closed_to_them`,
`a_store_others_can_write_to_is_refused`, `a_pack_others_can_write_to_is_not_read`,
`a_store_that_is_no_directory_is_refused`, `a_store_of_another_user_is_refused` (this one
only does anything as root; I ran it with `sudo` and it passed). End to end:
`harness/storeguards.py`.

**H3. Unsigned remotes were pulled from without asking. Fixed here for the remote that is
on `rustd-collate`; patch for `rustd-s3` below.** `rustd cache pull` and `rustd deps pull`
now refuse unless `RUSTD_REMOTE_UNSIGNED=1`, before anything is fetched. Only the exact
value `1` counts. Push is unchanged. Tests: `remote::tests::lists_nobody_signed_are_taken_only_when_asked_for`,
`harness/storeguards.py` (refused with empty, `0` and `true`; taken with `1`),
`harness/deps.py` and `harness/storefull.py` (each first checks the refusal).

**H4. A dependency archive could write outside its unit. Fixed.** `rustd deps pull`
unpacked tar archives from the remote. Entries were only checked by path prefix, so a
symlink entry followed by a file through it, or a hard link, could place files elsewhere,
and the boolean `unpack_in` returns for a skipped `..` path was ignored. Now only plain
files and directories of the unit are accepted and a skipped entry is an error. Test:
`deps::tests::a_unit_with_a_link_or_a_file_of_another_is_refused`.

### Medium

**M1. Stored bytes were trusted once the checksum matched. Fixed.** An entry that decodes
is not necessarily one that is safe to use: relocations could point past the end of the
code or the data, or name functions and data the entry does not have, and the code that
applies them indexes without checking. `Entry::decode` and `StoredCode` decoding now
validate every offset and index (`sound`) and return nothing for an entry that fails, which
is a cache miss and a recompile. Tests: `cached::tests::an_entry_that_names_what_it_lacks_is_no_entry`,
`code_of_llvm_that_names_what_was_not_asked_for_is_not_restored`.

**M2. A stored debug type could overflow the stack. Fixed.** `DebugType` is recursive and
postcard will follow nesting as deep as the bytes say. Decoding now stops at 128 levels,
and types deeper than that are not written either. Test:
`cached::tests::a_type_inside_too_many_others_is_not_read_and_not_kept`.

**M3. The pack reader added a length from the file without checking. Fixed.** `start + len`
is now a checked add followed by a checked slice. On 64-bit this could not overflow with a
32-bit length, on a 32-bit host it could. Covered by the pack fuzz loop.

**M4. Standard-library paths carried the user's home directory into the store and so into
a remote. Fixed.** This fell out of `storeguards.py`. Names of standard-library items are
meant to use `/rustc/<commit>/...` as rustc does, but the commit was read from a field of
rustup's manifest that 1.99.0 does not have, so the names silently fell back to
`/home/<user>/.rustup/toolchains/...`. Two effects: every pack held the user's home path
thousands of times, and two users never shared a standard-library instantiation. The
commit is now also read from the built `libstd` rlib. Tests:
`tests::the_commit_of_a_toolchain_is_the_one_its_library_has_its_sources_under` and
`storeguards.py`, which greps every byte of the store and of a pushed remote for the
checkout path and the home directory (1.6 MB each, 0 occurrences).

**M5. A file-name parser accepted names it should not. Fixed.** Found by the fuzz loop:
a name like `<hash>.pack.4242\0-17.tmp` was taken for one of the store's temporary files
(and so could be removed by collection). The writer suffix must now be digits and hyphens.
Test: `store::tests::no_name_breaks_the_telling_of_what_a_file_is`.

### Low, not changed

**L1. rustc's `StableCrateId` is 64 bits and names crates from outside the checkout.**
Registry crates and the standard library are named by rustc's own id, which is a 64-bit
SipHash with a public key. I left it, with this argument: the id is never all that tells
two crates apart. Every item name also has the path of its source file, which for a
registry crate ends in `registry/src/<index>/<name>-<version>/...`, and for the standard
library is `/rustc/<commit>/...`. To get code looked up under serde's names an attacker
needs a crate with serde's id *and* its files at serde's paths, and whoever can put files
there already owns the build. Inside the hashes of checkout crates the id is mixed in next
to the name and version, under SHA-256. This is an argument, not a test.

**L2. `own_crate_id` is 64 bits of a SHA-256.** It stands for a checkout crate that rustc
built nothing of, in two places: linker symbol names (`rustd.<id>.<path>`) and `TypeId`s,
where it sits in the slot rustc's 64-bit id would. Neither decides which stored code is
used: stable names of checkout crates use the full `ContentId`. A collision needs two
crates of one checkout with the same name hash and would be a duplicate symbol at link
time. Left at 64 bits because `TypeId` has to match rustc's layout.

**L3. The other non-cryptographic hashes do not cross a process.** `FxHasher` in
`inputs.rs` (file stamps), `main.rs` (settled-files check), `outline.rs` and
`codegen/patch.rs` (what changed since the last patch) all live and die inside one daemon
and only decide whether to redo work. `type_id.rs` uses rustc's 128-bit stable SipHash
because it has to produce the same `TypeId` rustc does. The CRC32 per record is an error
check behind the pack's SHA-256. Store keys, pack names, the LLVM code key and the
assembled-asm key are SHA-256.

**L4. The asm key does not length-prefix.** `asm::assemble` hashes `triple` then `text`
with nothing between. No supported triple is a prefix of another followed by valid
assembly, so I do not see a collision, but it should be made unambiguous the next time
that key changes.

**L5. A symlink in the store is followed.** Only the user can create one there, and the
target is checked with `fstat` like any file, so it has to be the user's and not writable
by others.

**L6. Windows has no owner or mode check.** The helper returns "fine" off Unix and relies
on the profile directory's ACL. Not tested.

## What an attacker can make a build run

With write access to the local store directory: anything. They compute the key of any
function (names are derivable from public sources), write a pack with their code under it,
and the next build links it. This is by design the same as write access to `target/`. The
fix is H2: rustd refuses to be in that position.

With write access to an unsigned remote, before this round: the same, for every machine
that pulls. After: nothing, unless the user set `RUSTD_REMOTE_UNSIGNED=1`.

With write access to a signed remote but no trusted key: they can delete, withhold or
roll back (serve an older signed list; lists carry no counter), which costs cache hits but
never gives wrong code, since every pack is named by its hash and every key includes the
content it was built from. They cannot add.

With read access to a remote: see "What one client can learn" below.

## Parsing untrusted bytes

cargo-fuzz needs nightly, so these are plain loops inside the unit tests. Each takes the
checked-in seeds (`crates/hir-ty/src/mir/codegen/corpus/`, four files, 36 KB on disk with
directories) plus seeds the test builds, and applies one to four mutations: byte flips,
edge values (0, 1, 0x7f, 0x80, 0xff...), truncation, insertion, and splicing of two seeds.
The generator is a fixed xorshift so every run is the same. A test-only global allocator
records the largest single allocation each decode asks for and the test fails if it
exceeds 1 MB plus 64 bytes per input byte, which is how "no unbounded allocation" is
checked rather than assumed.

Default run (20,000 mutations each, part of `cargo test`):

| Target | Accepted | Refused | Panics |
|---|---:|---:|---:|
| `Entry::decode` | 824 | 19,179 | 0 |
| `StoredCode` decode (LLVM build only) | 1,961 | 18,041 | 0 |
| pack reader | 131 whole | 19,873 not | 0 |
| store file names | all classified | | 0 |
| manifest | 20,000 inputs | | 0 |
| dependency key list | 20,000 inputs | | 0 |

Long run, `RUSTD_FUZZ_RUNS=2000000 cargo test --release -p hir-ty --features llvm --lib no_`:
entry 83,573 accepted and 1,916,430 refused; stored code 193,184 and 1,806,818; pack
11,759 and 1,988,245; 0 panics, 0 allocations over the bound, 4.7 seconds. Every accepted
entry is then walked the way the linker walks it (`follow`) to show that no offset or index
in it is out of range.

What fell out: M1, M2, M3 and M5 above. Accepted manifest lines are checked to be exactly
64 hex digits and `.pack`, with no path separator.

Not covered: the manifest and key-list loops do not honour `RUSTD_FUZZ_RUNS` and do not
measure allocation (they parse lines of a buffer that is already in memory). The S3 fetch
on `rustd-collate` reads a response of any size into memory; `rustd-s3` replaces that code
and its `read_small` has the same property for lists (see below).

## The `rustd-s3` branch

`origin/rustd-s3` (7b33ead6c2) has pushed its work and report, but it is still based on
27315323cb, before the format-2 store. Merging it here conflicts across `store.rs`,
`remote.rs` and `deps.rs` and would mean doing its port for it, so I did not. I applied
the following to a checkout of that branch and ran its tests (`cargo test --release -p
rustd signed`: 4 passed, including the new one). It should go in when that branch lands,
and the `Unsigned` type added to `remote.rs` and `deps.rs` here goes away in favour of it.

```diff
--- a/crates/rustd/src/remote/signed.rs
+++ b/crates/rustd/src/remote/signed.rs
@@ pub(crate) struct Trust {
     signer: Option<SigningKey>,
     trusted: Vec<VerifyingKey>,
+    unsigned: bool,
 }
@@ fn from_env
-        Ok(Trust { signer, trusted: trusted.collect::<anyhow::Result<_>>()? })
+        let unsigned = std::env::var("RUSTD_REMOTE_UNSIGNED").is_ok_and(|it| it == "1");
+        Ok(Trust { signer, trusted: trusted.collect::<anyhow::Result<_>>()?, unsigned })
@@ fn open
-        if self.checks() { self.verify(name, text) } else { Ok(split(text).0) }
+        if self.checks() {
+            return self.verify(name, text);
+        }
+        if !self.unsigned {
+            bail!(
+                "`{name}` is not taken: RUSTD_REMOTE_TRUST names no key, and whoever can write \
+                 to the remote decides what the build runs. Set RUSTD_REMOTE_UNSIGNED=1 if all \
+                 who can are trusted"
+            );
+        }
+        Ok(split(text).0)
@@ fn open_to_extend
-        if self.signer.is_some() { self.verify(name, text) } else { self.open(name, text) }
+        if self.signer.is_some() || self.checks() {
+            self.verify(name, text)
+        } else {
+            Ok(split(text).0)
+        }
```

`open` is the read path (`Remote::read_list`); `open_to_extend` is the push path and must
keep working unsigned, which is why it no longer goes through `open`. In the tests, add
`unsigned: false` to `trusting`, change the one assertion in
`a_list_is_taken_from_a_trusted_key_only` that expects an untrusting machine to read a
list into `is_err()`, and add:

```rust
#[test]
fn with_no_key_to_trust_a_list_is_taken_only_when_told_to() {
    let sealed = trusting(Some(1), &[]).seal("manifests/a", "one\n");
    for text in [sealed.as_str(), "one\n"] {
        let refused = trusting(None, &[]).open("manifests/a", text).unwrap_err();
        assert!(refused.to_string().contains("RUSTD_REMOTE_UNSIGNED=1"));
        let told = Trust { unsigned: true, ..trusting(None, &[]) };
        assert_eq!(told.open("manifests/a", text).unwrap(), "one\n");
        assert_eq!(trusting(None, &[]).open_to_extend("manifests/a", text).unwrap(), "one\n");
    }
    let told = Trust { unsigned: true, ..trusting(None, &[2]) };
    assert!(told.open("manifests/a", &sealed).is_err());
    assert!(told.open("manifests/a", "one\n").is_err());
}
```

Its `harness/remote.py` builds machines without `RUSTD_REMOTE_TRUST` in several places;
those need `RUSTD_REMOTE_UNSIGNED=1`, and the "without RUSTD_REMOTE_TRUST the forged pack
is taken" case becomes "is refused". I did not run that harness.

Three more things for that session, from reading its branch:

1. `Remote::read_small` reads a list of any size into memory, for both the directory and
   S3. Cap it (a few MB is generous for a list of hashes) before `String::from_utf8`.
2. `deps.rs` `unpack` there still calls `entry.unpack_in(dir)?` without looking at the
   result or the entry type. It needs the H4 fix from this branch; the two will conflict
   in that function and this branch's version is the one to keep.
3. Signed lists have no counter or date, so a remote can serve an old one. That costs
   hits, not correctness, but it also means a pack a signer has since stopped vouching for
   can be served forever. Worth a line in `REMOTE.md` at least.

## The daemon socket: findings to forward

Read at `origin/rustd-cargo` 13054e50d6 (`socket.rs`, `cargo/daemon.rs`, `cargo/client.rs`,
and the socket parts of `main.rs`). Nothing was edited. What is already right: the
directory is made 0700 and refused if it is not a directory, is another user's, or has any
group/other bit, using `symlink_metadata` so a link is not followed; the socket is 0600;
the peer's uid is checked (`SO_PEERCRED` on Linux, `getpeereid` elsewhere) before a byte
is read; the lock is taken before a stale socket is removed, so two daemons starting
together cannot unlink each other's socket; requests are capped at 16 MiB and 10 seconds;
each connection has its own thread so a slow one blocks nobody.

Findings:

1. **Medium. A daemon started with `--socket PATH` does not check PATH's directory.**
   `private_dir` is only called for the daemon's home and the worker directories. The
   plain `rustd … --socket /tmp/x.sock` path (`main.rs`, `Served::bind(path)`) binds
   wherever it is told. The server side is still safe on Unix because of the peer check.
   The client side is not: `rustd sync SOCKET` connects to whatever is at that path and
   sends its request and, for workers, the whole environment of the client process
   (`read_request_and_env`). In a shared directory another user can be listening there
   first. Either call `private_dir` on the socket's parent in both the daemon and the
   client, or have the client check the server's uid the way the server checks the
   client's.
2. **Medium. Between `bind` and `chmod 0600` the socket has the umask's mode.** Inside a
   0700 directory this does not matter. With finding 1 it is a window in which another
   user can connect; the peer check then turns them away, so this is defence in depth
   only. Setting the umask around `bind`, or binding inside a private directory always,
   closes it.
3. **Low. No write timeout.** `read_lines_within` sets a read timeout and then clears it.
   Nothing sets a write timeout, so a client that sends a request and never reads the
   reply holds its thread, and whatever the reply path holds, until it goes away. Same
   user only. `set_write_timeout` next to the read one.
4. **Low. No cap on connections.** One thread per connection, no limit. A same-user
   process can exhaust threads. Same user only, so low; a bounded pool or a counter would
   do.
5. **Low. The lock file and the logs are created with the umask.** `File::create(lock_of(socket))`,
   `daemon.log` and the workers' `log` get 0644 on a usual system. They sit in 0700
   directories so nobody else can reach them, but the logs hold project paths, compiler
   diagnostics and whatever a build prints. Create them 0600 so that they stay private if
   the directory is ever copied or its mode loosened.
6. **Low. `private_dir` checks only the last component.** `DirBuilder::recursive` creates
   missing parents 0700, but an existing parent that another user can write to lets them
   swap the directory between the check and the bind. In practice the parent is `$HOME` or
   `$XDG_RUNTIME_DIR`. Worth a sentence in the doc comment, or a check that each parent up
   to the home directory is the user's or root's and not writable by others.
7. **Low. Windows.** `private_dir` is `create_dir_all`, `only_the_user_connects` and
   `check_peer` do nothing, on the argument that the profile directory's ACL is inherited.
   That holds for the default location and not for a `--socket` or home override outside
   the profile. Untested there. At least refuse a path outside the profile, or say so in
   the docs.
8. **Info. The status reply lists every worker's project root, checkout roots and log
   path.** Same user only, given the peer check. See the next section.
9. **Info. Worker ids are eight bytes of a SHA-256 of the checkout.** They name a
   directory under the daemon's home and select which running worker serves a request.
   They do not reach store keys. Two checkouts of one user colliding would share a worker
   directory; 2^32 checkouts is not a real number, but widening to 16 bytes costs nothing.

## What one client can learn about another checkout

Across users, after this round, I found nothing:

- The store is 0700/0600 and refused otherwise, so another user cannot read it, list it,
  time it or run `rustd cache stats` on it. Before H2 they could do all four, and from the
  names in a pack learn the relative paths of the source files, item positions and the
  types functions were instantiated with.
- The socket turns away other users (on Unix), so status, diagnostics and build timing are
  not reachable.
- Logs live in the daemon's 0700 directory.

Across machines, through a remote, for anyone who can read it:

- Packs hold stable names: for checkout crates the content hash, file paths relative to
  the checkout (`app/src/main.rs`), item positions and the generic arguments of every
  instantiation; for registry crates the absolute path of the pusher's cargo registry,
  which has their home directory in it. Machine code is of course there too.
- The checkout's own path is not there. Code whose data names the checkout (through
  `env!("CARGO_MANIFEST_DIR")` and the like) is kept out of shared entries by an existing
  check, and `storeguards.py` now measures that: 0 occurrences of the checkout path or the
  home directory in the store and in the pushed remote for the stress workspace. That
  workspace has no registry dependencies, so the registry-path statement above is from
  reading the code, not from the harness.
- Whether a key is present tells a reader that someone built exactly that source. They
  have to have the source already to compute the key.

Between two checkouts of the same user, which is inside the trust boundary and was not
changed:

- `rustd cache stats` gives counts, sizes and how long ago the store was used, not
  contents.
- A cache hit is faster than a miss, so one checkout can tell that another built the same
  function from the same source.
- The daemon's status lists other projects' paths, and diagnostics and logs have source
  paths in them.

## What is left

- **Registry paths in names.** The pusher's cargo home appears in packs for registry
  crates, and two users with different homes do not share those instantiations. This is
  not a naming detail that can be changed alone: compiled code has the same absolute path
  in its panic locations, so sharing it under a path-free name would hand one user's paths
  to another inside the code. The real fix is to remap registry paths in both, as
  `--remap-path-prefix` does, and that has to agree with what rustc put in the rlibs the
  program links. Not done.
- **Signed lists are not on this branch.** They are on `rustd-s3`. Until it lands, the
  only choices here are refuse (the default) or `RUSTD_REMOTE_UNSIGNED=1`. The patch above
  is what makes the default right once it does.
- **Rollback of signed lists** (above).
- **Unbounded list and S3 response sizes** in the remote code, here and on `rustd-s3`.
- **Windows**: no owner or mode checks in the store, no peer check on the socket.
- **`a_store_of_another_user_is_refused` needs root** to do anything, so CI as a normal
  user does not exercise it.
- **L1 and L4** are arguments. If rustd ever names an outside crate without its path, L1
  stops holding.
- **Same-user isolation** (one checkout's daemon against another's) is not a goal and
  nothing here provides it.

## Validation

All of this is on Linux x86-64, with stable 1.99.0 and LLVM 20.1.8, on the head of this
branch. Nothing else was run.

Builds and checks:

    cargo fmt --all -- --check                                      clean
    cargo xtask tidy                                                exit 0
    cargo build -p rustd --release                                  builds
    LLVM_SYS_201_PREFIX=/usr/lib/llvm-20 CARGO_TARGET_DIR=target-llvm \
      cargo build -p rustd --release --features llvm                builds
    cargo test --release -p hir-ty -p hir-def -p parser -p syntax -p rustd
                                                                    948 passed, 0 failed, 1 ignored

The harnesses, from `harness/`, one backend after the other, with `RUSTD_BIN` at the
release binary and `RUSTD_BACKEND=cranelift`, then `RUSTD_BACKEND=llvm RUSTD_OPT=2
RUSTD_EMIT=fast`:

| Harness | Cranelift | LLVM |
|---|---|---|
| `difftest.py corpus --bin runner --crate cases --layouts` | MATCH, 0 of 892 layouts differ | same |
| `programs.py` | 22 of 22 | 22 of 22 |
| `stress.py` | 99 of 99, 0 restarts | 99 of 99, 0 restarts |
| `breakers.py` | 25 of 25, 0 open, 0 skipped | same |
| `unwinds.py` | 8 of 8 | 8 of 8 |
| `splices.py` | 71 of 71 | 71 of 71 |
| `asmedits.py` | 10 of 10 | 10 of 10 |
| `includes.py` | 9 of 9 | 9 of 9 |
| `worktrees.py` | 58 of 58 | 58 of 58 |
| `keys.py` | 36 of 36, 0 open | 36 of 36, 0 open |
| `debuginfo.py` | all right | all right |
| `deps.py` | 11 of 11 | 11 of 11 |
| `storekills.py --kills 100` | 100 kills, all verified | same |
| `storecorrupts.py --rounds 4` | 4 damaged stores rebuilt | same |
| `storefull.py` | 6 full disks, all built | same |
| `storeshared.py` | 180 builds | 180 builds |
| `storeguards.py` (new) | all right | all right |

`buildmodel.py` runs both backends itself and so needs the binary built with
`--features llvm`: 179 of 180 as expected, 0 WRONG. The one that is not is
`cross_aarch64` under LLVM, and it is not from this branch: rustd dies of a segmentation
fault inside LLVM's AArch64 instruction selection (`InstructionSelect::selectMachineFunction`
under `LLVMTargetMachineEmitToMemoryBuffer`), three runs of three, and the binary built
from `rustd-collate` at 0151c01218 does the same on this machine, two runs of two. I did
not look further, since it is in the code generation the ABI session owns. Two things
about it are worth forwarding: the harness prints a daemon killed by a signal as
`refused`, which hides a crash behind the word for a stated reason, and the matrix that
is green elsewhere must differ from this machine in its LLVM build.

Three things in the gate changed because of this round, and are said here so they are not
taken for noise:

- `storefull.py` failed under LLVM at first: its largest disk left 1 MB of room and the
  store of a cold build no longer filled it. The cause is the fix for standard-library
  paths: with `/rustc/<commit>` in place of the toolchain's directory, each of the 4,330
  paths in the LLVM store is 36 bytes shorter, and the store went from about 1.09 MB to
  0.94 MB (measured: 937,087 bytes in 16 files). The largest room is now 768 KB. The
  binary of `rustd-collate` passes the disk sizes with the old number, which is how I
  know it was this.
- `deps.py` failed at first on my own change: the check that an archive holds only
  files and directories refused the sparse files that `tar` writes for a build script
  with holes in it. A sparse file is now taken as a file; links are still refused.
- Run at the same time in one checkout, the two backends' gates disturb each other
  (`difftest.py` and `debuginfo.py` each failed once with "no linker line", when the
  other had just built the same corpus). Run one after the other, both pass. The numbers
  above are from the sequential run.
