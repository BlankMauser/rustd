# Remote hydration over S3

Branch `rustd-s3`, from `rustd-collate` at `27315323cb`, with `rustd-collate-wip` at
`0151c01218` (debug info, build model, disk cache) merged in. The numbers below are from
after the merge unless they say otherwise.

`rustd cache push REMOTE` and `rustd cache pull REMOTE` had an S3 path that had never been
run. This is what it took to make it real, what was measured, and what is still open.

**Real AWS S3 was not run.** There are no AWS credentials in the session this was done in.
Everything below was measured against MinIO (`RELEASE.2026-09-22T19-25-18Z`) and SeaweedFS
(4.48) on the same machine, over loopback. The harness runs against a real bucket with one
variable, `RUSTD_S3_REMOTE=s3://bucket/prefix`; nobody has done that yet.

## What changed

The S3 client is now the AWS SDK for Rust (`aws-config`, `aws-sdk-s3`) in place of
`rusty-s3` and `ureq`. The reason is credentials: `rusty-s3` signs with a key pair you hand
it, and the task asks for the sources the AWS tools read. With the SDK's default chain rustd
takes credentials from the environment, the shared config and credentials files (and a
named profile), web identity tokens, ECS container credentials and EC2 instance roles,
without rustd having any code for them. Of those, the environment and the shared files with a profile
were run; web identity, container and instance roles were not, because there is nothing here
that issues them.

The code is in `crates/rustd/src/remote.rs` and three files under `crates/rustd/src/remote/`:

- `s3.rs`: the S3 calls. Ranged GET, HEAD, conditional PUT for lists, multipart upload.
- `transfer.rs`: fetching an object into a file. Resuming, hashing, the limit on requests
  in flight, the counters.
- `signed.rs`: signed manifests.

`crates/rustd/src/deps.rs` (`rustd deps push|pull`, cargo's compiled units) moved onto the
same remote, so it has the same verification, retries and counters.

### On the store of the disk-cache branch

The merge put the remote on the format-2 store. What that means in the code:

- The remote goes through `StoreDir` for everything it does to the directory: `claim`
  before a push or a pull (so a store of an older format is emptied and one of a newer
  format is refused, as for a daemon), `adopt` before a push, `packs` for what is there.
- A pack's keys for the remote come from `pack_keys`, which is the store's own check of
  the pack (magic, name, every record's checksum), so a pack the store would discard is
  never sent. The keys are the ones on disk, which already have the store's identity
  (format, rustd build, toolchain) hashed in.
- There is one set of crash-safe writes. A fetched pack arrives in pieces, so it cannot be
  handed to `write_file_whole` as bytes; `store.rs` now has `name_file_whole`, which is the
  second half of its `write_whole` (sync, rename, sync the directory), and `write_whole`
  itself is written with it. The remote's partial file is `<hash>.pack.fetch.tmp`, which
  the store counts as one of its temporary files: `cache stats` shows it, and a collection
  removes it once it is a minute old and nobody holds it. The directory remote writes its
  objects with `write_file_whole` and `name_file_whole` too. The `.partial` files and the
  rename of the first version are gone.
- A manifest line is now `PACK SIZE KEYS FORMAT`. A pull passes over packs listed with
  another format without fetching them ("N of another format left"). A line that claims
  this format for a pack that starts with another magic is not believed: the pack is
  rejected after the fetch and never gets its name.
- `RUSTD_CACHE_MAX`: a pull fetches the listed packs that fit under the limit and says how
  many do not ("N left that do not fit under RUSTD_CACHE_MAX"), then runs the store's
  collection, which evicts the packs unused the longest if the store is now over. A pulled
  pack is a pack like any other after that. One that is evicted is missing again, so the
  next pull fetches it.
- A collection also makes one pack of many small ones, under a new name. So "is this
  listed pack here" cannot be asked by name alone, or a machine would fetch its own
  history again at every start. When the store is not empty and a listed pack is missing
  by name, the pull fetches its key list and passes the pack over if every key is in the
  store already (`StoreDir::keys`). A fresh machine has no keys and makes no such request.
- Lazy hydration keeps the key lists in memory. Nothing that is not the store's is written
  into the store's directory, so `cache stats` and `cache verify` count the same things
  after a pull as before.
- `rustd cache stats|gc|verify` are the disk-cache branch's, unchanged. `cache keygen` is
  next to them.

New public items in `hir-ty`'s `codegen`, since that is an architectural change in this
tree: `name_file_whole`, `pack_keys`, `pack_format`, `StoreDir::keys`, and
`fetch_missing_with`/`FetchMissing` for lazy hydration.

### What the AWS SDK costs

Measured on this machine (8 cores), `cargo build -p rustd --release` into an empty target
directory with the crates already downloaded, Cranelift only:

| | crates in rustd's tree | clean build | binary | stripped |
| --- | --- | --- | --- | --- |
| `rustd-collate-wip` (`rusty-s3`, `ureq`) | 284 | 91.7 s, 85.5 s | 45.3 MB | 32.5 MB |
| the SDK with its default features | 380 | 129.2 s | 68.6 MB | 47.3 MB |
| the SDK as it is on this branch | 347 | 113.5 s | 62.6 MB | 42.5 MB |

So the SDK adds 63 crates, about 25 s to a clean build (roughly 30%) and 10.1 MB to the
stripped binary (31%). The first row was built twice and the other two once, so the build
times are good to a few seconds.

The default features cost more than that, and the branch turns three of them off:
`rustls` on `aws-sdk-s3` is a second, older HTTP and TLS stack (hyper 0.14, rustls 0.21)
next to the one it actually uses; `default-https-client` brings aws-lc, a C library built
with cmake; `sigv4a` is only for multi-region access points. The client is built by hand
instead, hyper 1 over rustls with ring, both of which `ureq` had in the tree already. TLS
through that client was run against a MinIO with a certificate (see the harness), not
against AWS.

What is left of the cost is mostly generated code: the S3 client, STS for web identity
and role credentials, and the two SSO clients. Dropping the `sso` feature saves another
1.3 MB stripped and 3 crates; it is kept because `aws sso login` is how most people get
credentials on a laptop. If 10 MB is too much, the alternative is `rusty-s3` for signing
plus `aws-config` only for credentials, or credentials from the environment only as
before. That would lose nothing that was measured here, but rustd would own the request
code (multipart, retries, error parsing) that the SDK owns now.

The remote is async on one tokio runtime of two worker threads, built once in a process.
The daemon's own threads are synchronous and wait on that runtime from outside; nothing
calls `block_on` from inside a task.

### Layout of a remote

    packs/<sha256 of the pack>.pack
    keys/<sha256 of the key list>          the 32-byte keys of one pack, back to back
    manifests/<project>                    one line a pack: PACK SIZE KEYS FORMAT
    units/<sha256>, deps/<project>         cargo's units, the same way

### Address and configuration

    s3://bucket/prefix?endpoint=URL&region=NAME&profile=NAME&path-style=true|false

Anything the address leaves out comes from where the AWS tools look: `AWS_ENDPOINT_URL`,
`AWS_REGION`, `AWS_PROFILE`, `~/.aws/config`. An unknown option is refused by name. With an
endpoint of its own rustd addresses the bucket by path unless told `path-style=false`,
because that is what most services that are not AWS need; without one it does what the SDK
does for AWS.

| Variable | Meaning | Default |
| --- | --- | --- |
| `RUSTD_REMOTE` | the remote the daemon pulls from before a build and pushes to after | none |
| `RUSTD_PROJECT` | which manifest | `default` |
| `RUSTD_REMOTE_JOBS` | requests in flight at once | 8 |
| `RUSTD_REMOTE_TIMEOUT` | seconds a request may go without an answer, or a body without a byte | 30 |
| `RUSTD_REMOTE_READONLY` | never write to the remote | off |
| `RUSTD_REMOTE_LAZY` | fetch a pack when a build first asks for a key in it | off |
| `RUSTD_REMOTE_SIGN` | file with the key this machine signs manifests with | none |
| `RUSTD_REMOTE_TRUST` | public keys this machine takes manifests from | none |

In read-only mode `cache push` and `deps push` fail with "RUSTD_REMOTE_READONLY is set:
nothing is sent", and the daemon does not push.

Credentials are never printed. rustd does not hold them; the SDK does. Errors are printed
as the service's error code and message, or the HTTP status, or the chain of causes of a
transport error, none of which contain the request's headers. The harness greps everything
rustd printed in the whole run for the secret key (and the session token when run against a
real bucket), forwards and reversed.

## Robustness

**Fetch.** An object is written to `<name>.fetch.tmp` under an exclusive lock, never to its
final name. When the body breaks off, rustd asks again with `Range: bytes=N-` from the byte
it stopped at, up to 6 times in a row without getting further, waiting 250 ms, 500 ms, ...
8 s in between; getting any bytes starts the count again. When the object is whole, its
SHA-256 is compared with the name it was listed under, and only then is it synced and
renamed to `<hash>.pack` by the store's `name_file_whole`. So a pack file in the store is
always a whole, verified pack: a killed rustd or a killed server leaves the temporary file
or nothing. The next process to run picks it up and fetches only the rest, unless a
collection got to it first (the store removes temporary files a minute after their last
byte). If what is in it turns out not to be the start of this object (the hash does not
match at the end), it is thrown away and fetched once from the start before the object is
rejected.

**Small objects** (manifests, key lists, the HEAD before a send, the conditional write of a
manifest) are asked for again on the same schedule as a pack is resumed: up to 6 more times,
250 ms to 8 s apart, on top of the SDK's own attempts. A conditional write that is made
again after its answer was lost finds its own bytes there, is told it lost, and the read
that follows sees there is nothing left to add.

**What is not tried again.** A refusal (any 4xx but 408 and 429: wrong credentials, no such
bucket), an object a list names that is not there, and an error writing the local file
(a full disk) are final. They fail at once, and the temporary file is removed: there is
nothing in it worth keeping when the disk is full. A TLS failure such as an untrusted
certificate is not told apart from a network failure and takes the whole 16 s to fail.

**Send.** A file of 16 MiB or more goes up as a multipart upload in 8 MiB parts, with at
most `RUSTD_REMOTE_JOBS` requests in flight across everything the process is doing. A part
that fails is sent again by the SDK (three attempts, exponential backoff with jitter). If
the upload fails it is aborted, and a push first aborts uploads an earlier process left
unfinished under the same key. The manifest is written last, after every pack and key list
it names is in the bucket, with `If-Match`/`If-None-Match` so that two machines pushing at
once do not undo each other (up to 8 rounds of read, extend, write).

**Timeouts.** Connecting: 5 s at most. An attempt with no answer, or a read with no bytes:
`RUSTD_REMOTE_TIMEOUT`. An upload attempt gets that plus a second for every 64 KiB, so a
slow link is not mistaken for a dead one.

**Failures are counted.** Every command ends with a line like

    remote: 15 requests (0 tried again, 0 transfers resumed), 0 bytes received, 42336756 sent, 0 objects rejected

and a push or pull in which anything failed prints each failure and exits non-zero with
"N failed, M of them rejected". In the daemon a remote that fails is not an error to the
build: it says so and compiles what it would have fetched.

## Trust

There are two different questions and they have different answers.

**Can the storage change the code?** No. A pack is named by the SHA-256 of its bytes and
is hashed after the fetch and before the rename, so a bucket (or a proxy, or a bad disk on
the way) that returns other bytes for a name gets them thrown away, counted under "objects
rejected", and reported: "rejected packs/X.pack: its N bytes hash to Y, not to X". A machine
also hashes each of its own packs before sending it and refuses to send one that a bad disk
spoiled.
Key lists and cargo units are named and checked the same way. The store checks a CRC32 of
each record again when it reads it. After a rejection the build compiles the functions
itself, so the result is the code a local build gives.

**Can the storage make rustd use code for the wrong thing?** A cache entry's key is a
SHA-256 over the backend and optimisation level, the target triple, whether the build has
debug info and overflow checks, and the function's stable name; the store then hashes that
together with its format, the rustd executable's own hash and the toolchain's commit before
it is written to disk. The stable name holds the function's path, its
generic arguments spelled out, and for the crate it is in either rustc's stable crate id
(dependencies: name, version and the metadata cargo passes) or a hash of the crate's source
(crates of the checkout). So an entry is only ever found by a machine running the same
rustd binary against the same toolchain, for the same backend and target, for the same instance of the same crate
source. A remote cannot move an entry to another key without changing the pack's bytes and
so its name.

**What is left is the manifest**, which is the one mutable object and says which packs to
fetch. Somebody who can write to the bucket can upload a well-formed pack of their own
making, with wrong code under real keys, and list it; its name is honestly the hash of its
bytes. Content addressing cannot catch that. Signing does: with `RUSTD_REMOTE_SIGN` a
machine signs each manifest it writes (Ed25519, over the manifest's object name and its
body, so one project's manifest cannot be replayed as another's), and with
`RUSTD_REMOTE_TRUST` a machine refuses any manifest that is not signed by a key it lists.
`rustd cache keygen FILE` makes a key and prints the public half.

So the honest statement is: with `RUSTD_REMOTE_TRUST` set, nobody without a trusted signing
key can make rustd emit different code than a local build, whatever they do to the bucket.
Without it, whoever can write to the bucket is trusted, the same as with sccache or a cargo
registry mirror. A trusted signer is trusted completely; rustd does not recompile to check
them. One more assumption sits under all of it and predates this branch: two machines with
the same rustd binary compile the same key to equivalent code. That is what the local store
already relies on between workspaces.

Tested by the harness, on both servers:

- pack truncated, extended, emptied, one byte flipped: rejected, reported, no pack in the store;
- a different valid pack stored under A's pack name: rejected, B compiles its own, and its
  program prints what cargo's build prints;
- a pack the manifest lists that is gone: reported as "is listed and is not there", exit non-zero;
- with trust on: a manifest forged under A's old signature, one with no signature, and one
  signed by a key B does not list are each refused, nothing is fetched, and the program is
  the same;
- a machine that trusts keys but has none to sign with refuses to push.

## Lazy hydration

With `RUSTD_REMOTE_LAZY=1` the daemon fetches only the key lists (32 bytes a function)
before a build. When the store is asked for a key it lacks and a listed pack holds it, the
compiling thread fetches that pack, verified as above, and looks again. A pack nobody asks
of is never fetched; the harness checks that.

It did not win in what was measured (below): 9.8 s against 9.4 s for ripgrep, 98.2 s
against 92.0 s for cargo. A fresh machine building the same workspace wants every pack, so
lazy fetches the same bytes plus the key lists, and fetches packs one after another as
threads trip over them instead of eight at a time. Where it should win is a manifest that
holds much more than one build needs, such as one project name shared by many workspaces or
a long history of pushes. That is an inference; the only evidence is the harness case where
an unused pack is not fetched.

## Measurements

Machine: 8 cores, 31 GB, Linux x86-64. Cranelift backend. MinIO on loopback behind the
harness's counting proxy, so transfer times are a lower bound; they say nothing about a real
network. Each "machine" is its own HOME, checkout, cargo target directory and rustd store
in a fresh directory. They are not separate containers. One run each, no repeats, so read
differences of a few percent as noise.

"To a running program" is everything a machine with a checkout and a toolchain does: pull
cargo's units if any, `cargo rustc` for the dependencies and link line, rustd's build, run
the binary. Crates.io sources were already in `~/.cargo` for all of them.

    cd harness
    RUSTD_BIN=.../rustd python3 remote.py --measure ~/work/rg-a -p ripgrep --bin rg \
        --crate rg,grep,grep_cli,grep_matcher,grep_printer,grep_regex,grep_searcher,globset,ignore
    RUSTD_BIN=.../rustd python3 remote.py --measure ~/work/cargo-a -p cargo --bin cargo \
        --crate cargo,cargo_util,cargo_platform,cargo_util_schemas,crates_io,rustfix,cargo_credential

ripgrep 14.1.1 (9 crates by rustd; 22,340 functions compiled cold):

| Fresh machine B | To a running program | cargo | rustd | Functions compiled | Requests | Bytes down |
| --- | --- | --- | --- | --- | --- | --- |
| no remote | 17.3 s | 4.9 s | 12.4 s | 22,340 | 0 | 0 |
| rustd's packs | 14.5 s | 4.9 s | 9.6 s | 0 | 2 | 51,804,757 |
| packs and cargo's units | 9.4 s | 2.0 s | 7.2 s | 0 | 44 | 103,610,692 |
| the same, lazy | 9.8 s | 2.1 s | 7.5 s | 0 | 45 | 104,396,292 |

Machine A's push: 1 pack, 51,804,616 bytes in 418 ms, 17 requests (a multipart upload);
41 cargo units, 51,802,181 bytes in 402 ms, 85 requests. Pulling the pack took 180 ms.

cargo 0.92.0 (7 crates by rustd; 226,180 functions compiled cold, 3,599 of 3,602 functions
of the workspace; the other three are refused by rustd and compiled by rustc, as on the base branch):

| Fresh machine B | To a running program | cargo | rustd | Functions compiled | Requests | Bytes down |
| --- | --- | --- | --- | --- | --- | --- |
| no remote | 152.0 s | 42.8 s | 109.1 s | 226,180 | 0 | 0 |
| rustd's packs | 114.1 s | 42.6 s | 71.5 s | 0 | 17 | 1,066,942,095 |
| packs and cargo's units | 92.0 s | 25.5 s | 64.9 s | 0 | 405 | 1,496,922,653 |
| the same, lazy | 98.2 s | 25.7 s | 70.9 s | 0 | 421 | 1,504,101,885 |

Machine A's push: 16 packs, 1,066,939,839 bytes in 3.7 s, 241 requests; 387 units,
429,944,713 bytes in 3.2 s, 777 requests. The bucket ends with 421 objects, 1,504,101,885
bytes. Pulling the 16 packs took 3.1 to 3.2 s.

In every row the program printed the same as the one built without the remote.

These are from after the merge. Before it, on the format-1 store, the same commands gave
15.2, 13.1, 8.6 and 8.7 s for ripgrep and 132.7, 105.7, 80.9 and 82.9 s for cargo, with
929 MB of packs for cargo instead of 1,067 MB. The merged base compiles more slowly with
no remote at all (10.6 s to 12.4 s of rustd for ripgrep, 89.7 s to 109.1 s for cargo) and
its packs are larger; the base branch now keeps debug info in an entry. The remote is not
what changed there.

What the numbers say: the remote removes all of rustd's code generation (0 functions
compiled) and that is worth 3 s of 17 for ripgrep and 38 s of 152 for cargo. With cargo's
units as well it is 17.3 s to 9.4 s and 152.0 s to 92.0 s. What is left is not transfer,
which is 0.4 s and under 5 s. It is rustd loading the workspace and walking every instance
to find its key (the "cold:" line still reads 4.0 s for ripgrep and 54 s for cargo with
nothing to compile), and cargo running build scripts and proc macros. Those are the next
thing to cut and are not a remote problem.

The store is large for what it holds: 1,067 MB of packs for cargo. Packs are not compressed
on the wire or at rest. On a real network at, say, 50 MB/s that is 21 s of the 60 s saved,
so compression would matter there. That sentence is arithmetic, not a measurement.

## The harness

`harness/remote.py` with `harness/s3fake.py`. It starts MinIO and SeaweedFS (found through
`RUSTD_MINIO`, `RUSTD_WEED`, PATH or `~/s3tools`), each behind a proxy of its own that
counts requests and bytes and can answer with a status, reset the connection, cut a response
or a request body at a byte, stall, or throttle. It builds a small generated workspace on
machine A, and then checks, 131 checks in all across the two servers:

- the round trip: A compiles and pushes, B with another HOME and an empty store pulls,
  compiles 0 functions, and prints what cargo's build prints; a second push sends nothing;
  every object in the bucket hashes to its name;
- configuration: read-only, a profile in the shared files, `AWS_PROFILE`,
  `AWS_ENDPOINT_URL`, region and path style, the bucket in the host name (MinIO, through
  `bucket.localhost`), a wrong secret, an unknown option, no secret in any output;
- TLS (MinIO only): a second MinIO behind a certificate signed by an authority the harness
  makes with `openssl`; a 20 MiB pack goes up and comes down with the authority trusted
  through `SSL_CERT_FILE`, and without it the pull is refused and says "invalid peer
  certificate";
- the store after a pull: `cache verify` and `cache stats` count exactly what was fetched;
  packs evicted with `cache gc --max 0` are fetched again; with `RUSTD_CACHE_MAX` a pull
  fetches the three packs of six that fit and says so; into a store that already holds two
  old packs it fetches three and the two old ones are evicted; 24 small packs that a
  collection made one of are not fetched again; a pack of format 1 listed as format 1 is
  left with no request made for it, and the same pack listed as format 2 is rejected;
- a fetch killed half way leaves a store that verifies and counts the kept bytes as one
  temporary file;
- multipart with bounded concurrency, and rustd's request count equal to the proxy's;
- 503 twice, 503 on a part, on completion and on the manifest, 503 forever;
- connection dropped mid-body twice, closed with no answer twice, dropped mid-part;
- a stall longer than the timeout, an answer slower than the timeout;
- rustd killed mid-fetch, the next run fetching only the rest, a stale temporary file of
  something else;
- two processes fetching into one store; twelve packs with three requests at a time;
- the server killed mid-fetch and restarted, killed and left dead, killed mid-push, and
  each of those recovered afterwards with no unfinished upload left in the bucket;
- the poisoning and signing cases listed under Trust;
- lazy: nothing compiled, same output, the unused pack not fetched.

    cd harness && RUSTD_BIN=../target/release/rustd python3 remote.py

Takes about seven minutes with both servers. With the code and the harness as pushed it
was run three times in a row under Cranelift and once under LLVM and gave 131/131 each
time (6m33s, 6m55s, 6m52s; 7m00s). The first run after the merge gave 129: the check for a
service slower than the timeout allowed 30 s and now takes 32, because the manifest is
asked for again like a pack. Its bound is 45 s now. Before the merge the harness had 111
checks and its last three runs gave 111/111.

One check is conditional. MinIO reads again 1.3 s after a kill and a fetch always rides
that out. SeaweedFS took between 14 and 23 s to read again in these runs, on either side of
the 16 s rustd waits for a byte. So the harness times how long the server was away: under
10 s the fetch has to succeed with at least one resume; over that it may either succeed, or
fail with no pack in the store and succeed when run again. Before that was written down the
check failed on SeaweedFS in two runs of eight.

Against a real bucket: `RUSTD_S3_REMOTE=s3://bucket/prefix python3 remote.py`, with
credentials wherever the AWS tools would find them. That runs the round trip, the
configuration, poisoning, signing and lazy checks, and skips the ones that need the proxy
or need to kill the server. **Not run.**

## Regression gate

Run on Linux x86-64 after the merge, under `RUSTD_BACKEND=cranelift` with the Cranelift-only
release binary and under `RUSTD_BACKEND=llvm RUSTD_OPT=2 RUSTD_EMIT=fast` with the
`--features llvm` binary (LLVM 20.1). Both gave the same results:

| Script | Result |
| --- | --- |
| `difftest.py corpus --bin runner --crate cases --layouts` | MATCH, 803/803 functions from rustd, 0 of 892 layouts differ |
| `programs.py` | 22 of 22 |
| `stress.py` | 99 of 99, 0 restarts |
| `breakers.py` | 25 of 25, 0 open, 0 functions skipped |
| `unwinds.py` | 8 of 8 |
| `splices.py` | 71 of 71 |
| `asmedits.py` | 10 of 10 |
| `includes.py` | 9 of 9 |
| `worktrees.py` | 58 of 58 |
| `keys.py` | 36 of 36, 0 open |
| `debuginfo.py` | all right |
| `storekills.py --kills 100` | PASS, 100 kills |
| `storecorrupts.py --rounds 4` | PASS, 4 damaged stores |
| `storefull.py` | PASS, 6 full disks |
| `storeshared.py` | PASS, 180 builds |
| `remote.py` | 131/131 |

The table is from the last run of everything, on the code as pushed. An earlier run of the
same scripts on the merge, before the last two fixes, gave the same counts except for
`storefull.py` under LLVM (below).

`buildmodel.py` runs with the `--features llvm` binary under `RUSTD_BACKEND=cranelift`, as
in CI, and does both backends itself: **179 of 180 as expected, 0 WRONG, and the script
exits 1.** The row is `cross_aarch64` under LLVM: refused, with nothing after "resolve: 22
crates" in what it printed. It was the same row in both full runs here. Run alone, or with
two other cases, that row passes. The CI run of `rustd-collate-wip` itself (0151c01218)
fails `buildmodel.py` on the same row with the same text, so it came in with the merge and
is not the remote; it looks like something that depends on what else is running. It is not
fixed here.

`storefull.py` failed once under LLVM before it passed: a pull into a full disk left two
`.fetch.tmp` files behind. The write of the last bytes had gone through and the sync
before the rename had not, and that path did not remove the file. It does now, and the
script passed three times under LLVM and once under Cranelift after the fix.

`cargo fmt --all -- --check` is clean. `cargo test --release -p hir-ty -p hir-def -p parser
-p syntax -p rustd`: 511, 22, 330, 15 and 56 passed, 0 failed.

### CI

The Linux job of `.github/workflows/rustd.yaml` now runs `remote.py` after the store
harnesses, under both backends. The runner can start MinIO without privileges: MinIO is one
statically linked program, and the job takes it out of the `cgr.dev/chainguard/minio` image
(pinned by digest) with `docker create` and `docker cp`, which the runner's user may do
without sudo. The old ways to get it are gone: `minio/minio` on Docker Hub and the download
on dl.min.io both refuse. SeaweedFS is not in CI; the harness says "left out" and runs the
MinIO leg, which is 67 checks.

The CI run of the pushed branch (run 39040145198): the MinIO step passed, and `remote.py`
gave 67/67 under both backends on the runner. The two harness jobs are red all the same,
for two things that are red on `rustd-collate-wip`'s own run (39031491164) too:
`debuginfo.py` ("wrong: 4" under both backends on the runner; it passes here) and
`buildmodel.py` (the row above). Everything else in both jobs passed, and `fmt and tests`
is green.

## What is still open

- **Real AWS has not been run.** The SDK is the same code against AWS as against MinIO, but
  virtual-hosted addressing against real DNS, IAM denial messages, and role credentials
  (web identity, ECS, EC2) are untested. Needs credentials.
- **The machines were directories, not containers,** and the network was loopback. Transfer
  times are best cases.
- **Packs are not compressed.** See above; 929 MB for cargo.
- **Nothing is ever deleted from a remote.** Manifests only grow. There is no garbage
  collection and no expiry, and lazy hydration is the only thing that keeps a long manifest
  from costing every fresh machine its whole history.
- **Lazy fetches block a compiling thread each** and are not batched; it did not win here.
- **Without `RUSTD_REMOTE_TRUST`, a writer to the bucket is trusted.** That is a choice of
  default, not a bug, but it should be decided before this is open-sourced whether an
  unsigned remote needs an explicit opt-in.
- **The lazy hook is one per process** (`fetch_missing_with` in `store.rs`, first call
  wins). That fits a daemon with one remote; it would have to become part of the store if a
  daemon ever serves two.
- **A remote holds the same code twice once a store has merged packs.** A machine pushes
  its small packs, later makes one pack of them and pushes that too. A pull passes over
  what it already has by key, but a fresh machine fetches both. It is bounded by the 64 MB
  a merged pack may be, per merge. The fix is on the push side (do not send a pack whose
  keys are all listed already) and costs a key-list request per listed pack, or a remote
  collection, which does not exist.
- **Telling a merged pack by its keys reads the store.** When a listed pack is missing by
  name and the store is not empty, the pull reads every local pack once to collect its
  keys and makes one small request per missing pack. For a store that has merged its own
  history that happens at every start. Not measured on a large store.
- **A TLS failure waits out the whole backoff** (about 16 s) before it is reported, since
  it is not told apart from a network failure.
- **A partial fetch is resumed only if a collection has not removed it**, which the store
  does a minute after its last byte when nobody holds it.
- **One failed pack stops the rest.** When a fetch runs out of resumes the remote is taken
  to be down and the other transfers of that process stop rather than each waiting out its
  own 16 s. The next run resumes them, and the daemon's push after a build tries afresh.
- **The daemon talks to the remote twice:** a pull when it starts and a push after its
  first build. A daemon that stays up for a day does not see what others push meanwhile
  and does not push what later edits made it compile. `rustd cache push|pull` can be run
  beside it.
- **Windows and macOS were not run.** The partial-file lock and the rename are std calls
  that exist there; the harness is Linux only.
