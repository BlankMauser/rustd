//! Keeping a store in step with a place other machines reach: `rustd cache push REMOTE` and
//! `rustd cache pull REMOTE`, and the same done by the daemon when `RUSTD_REMOTE` names one.
//! The commands on the local store alone, `rustd cache stats|gc|verify`, are here with them.
//!
//! A remote is a directory or `s3://bucket/prefix`. It holds sealed packs under the hash of
//! their bytes, the keys of each pack under the hash of those, and one manifest a project:
//! the packs its builds were served from. A push sends the packs the manifest lacks and
//! adds them to it; a pull fetches the ones it lists that are not here. With
//! `RUSTD_REMOTE_LAZY` the daemon fetches the keys only, and a pack when a build first asks
//! for a key it holds. A daemon that stays goes on in `keeper.rs`: it pulls again when a
//! build lacked something and the manifest has changed, and pushes on a schedule and when
//! it is told to end.
//!
//! What is fetched becomes a pack of the store like any other: it is written the way the
//! store writes its own, counts toward `RUSTD_CACHE_MAX`, and can be evicted and fetched
//! again. A pull fetches no more than fits under that limit.
//!
//! Nothing fetched is used before its bytes hash to the name it was listed under, so a
//! remote can withhold code and cannot change it. What it could change is the list, which
//! is what `signed.rs` is for. `RUSTD_REMOTE_READONLY` keeps a machine from writing at all.

mod collect;
mod keeper;
mod s3;
mod signed;
mod transfer;

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    path::{Path, PathBuf},
    sync::{Arc, OnceLock, atomic::Ordering},
    time::{Duration, Instant, SystemTime},
};

use anyhow::{Context, bail};
use futures_util::{StreamExt, stream};
use hir_ty::mir::codegen::{
    STORE_FORMAT, StoreDir, limit_from_env, name_file_whole, pack_format, pack_keys, parse_size,
    write_file_whole,
};
use rustc_hash::FxHashMap;
use tokio::{io::AsyncSeekExt, runtime::Runtime};
use url::Url;

use s3::{S3, Written};
use signed::Trust;
use transfer::{Body, Object, Version};
pub(crate) use transfer::{Digest, Fetched, Traffic};

/// How often a list is read again and written again after another machine wrote first.
const LIST_WRITES: usize = 8;
/// An object that is there and that no list names yet is taken as sent for this long after
/// it was written. Past that it is sent again, so that a collection of the remote, which
/// goes by when an object was written, does not take it from under a push.
const REUSE_FOR: Duration = Duration::from_secs(60 * 60);

pub(crate) struct Remote {
    place: Place,
    traffic: Arc<Traffic>,
    /// How long a request may go without an answer, or a body without a byte.
    patience: Duration,
    read_only: bool,
    trust: Trust,
}

enum Place {
    /// A directory, such as a mount shared by the machines.
    Dir(PathBuf),
    S3(Box<S3>),
}

impl Remote {
    /// The remote `spec` names, as the environment has this machine use it:
    /// `RUSTD_REMOTE_JOBS` requests at a time (8), `RUSTD_REMOTE_TIMEOUT` seconds without
    /// an answer (30), `RUSTD_REMOTE_READONLY` to never write, and the keys of `signed.rs`.
    pub(crate) async fn open(spec: &str) -> anyhow::Result<Remote> {
        let number = |name: &str, default: u64| match std::env::var(name) {
            Ok(value) => value.parse().with_context(|| format!("{name} is not a number")),
            Err(_) => Ok(default),
        };
        let traffic = Arc::new(Traffic::new(number("RUSTD_REMOTE_JOBS", 8)? as usize));
        let patience = Duration::from_secs(number("RUSTD_REMOTE_TIMEOUT", 30)?.max(1));
        let place = if spec.starts_with("s3://") {
            let spec = Url::parse(spec).context("the address of the remote is not a URL")?;
            Place::S3(Box::new(S3::open(&spec, patience, traffic.clone()).await?))
        } else {
            Place::Dir(PathBuf::from(spec))
        };
        let read_only = std::env::var_os("RUSTD_REMOTE_READONLY").is_some_and(|it| it != "0");
        Ok(Remote { place, traffic, patience, read_only, trust: Trust::from_env()? })
    }

    /// An error when this machine is not to write to the remote, saying why.
    pub(crate) fn may_write(&self) -> anyhow::Result<()> {
        if self.read_only {
            bail!("RUSTD_REMOTE_READONLY is set: nothing is sent");
        }
        match self.trust.cannot_sign() {
            Some(why) => bail!("{why}: what this machine sent would not be trusted"),
            None => Ok(()),
        }
    }

    /// The object under `name` from `offset` on, or `None` when there is nothing under it.
    async fn read_from(&self, name: &str, offset: u64) -> anyhow::Result<Option<Body>> {
        match &self.place {
            Place::Dir(dir) => {
                self.traffic.operations.fetch_add(1, Ordering::Relaxed);
                match tokio::fs::File::open(dir.join(name)).await {
                    Ok(mut file) => {
                        file.seek(std::io::SeekFrom::Start(offset)).await?;
                        Ok(Some(Body::File(file)))
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                    Err(e) => Err(e).with_context(|| format!("fetching {name}")),
                }
            }
            Place::S3(s3) => Ok(s3.read_from(name, offset).await?.map(Body::S3)),
        }
    }

    /// The whole of a small object and the version it was read at, asked for again for as
    /// long as a pack is.
    async fn read_small(&self, name: &str) -> anyhow::Result<Option<(Vec<u8>, Version)>> {
        let _permit = self.traffic.permit().await?;
        self.patiently(|| self.read_small_once(name)).await
    }

    async fn read_small_once(&self, name: &str) -> anyhow::Result<Option<(Vec<u8>, Version)>> {
        match &self.place {
            Place::Dir(dir) => {
                self.traffic.operations.fetch_add(1, Ordering::Relaxed);
                match tokio::fs::read(dir.join(name)).await {
                    Ok(bytes) => {
                        let version = Version(Some(Digest::of(&bytes).to_string()));
                        Ok(Some((bytes, version)))
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                    Err(e) => Err(e).with_context(|| format!("fetching {name}")),
                }
            }
            Place::S3(s3) => s3.read_small(name).await,
        }
    }

    /// The object under `name`, if there is one.
    async fn found(&self, name: &str) -> anyhow::Result<Option<Object>> {
        let _permit = self.traffic.permit().await?;
        self.patiently(|| self.found_once(name)).await
    }

    async fn found_once(&self, name: &str) -> anyhow::Result<Option<Object>> {
        match &self.place {
            Place::Dir(dir) => {
                self.traffic.operations.fetch_add(1, Ordering::Relaxed);
                match tokio::fs::metadata(dir.join(name)).await {
                    Ok(found) => Ok(Some(Object {
                        name: name.to_owned(),
                        len: found.len(),
                        written: found.modified()?,
                    })),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                    Err(e) => Err(e).with_context(|| format!("looking for {name}")),
                }
            }
            Place::S3(s3) => s3.found(name).await,
        }
    }

    /// Whether the object under `name` is there with `len` bytes and need not be sent. One
    /// that has been there for longer than [`REUSE_FOR`] is sent again, which tells a
    /// collection of the remote that it is wanted.
    async fn holds(&self, name: &str, len: u64) -> anyhow::Result<bool> {
        let fresh = |it: &Object| it.written.elapsed().is_ok_and(|age| age < REUSE_FOR);
        Ok(self.found(name).await?.is_some_and(|it| it.len == len && fresh(&it)))
    }

    /// The objects whose names start with `prefix`, which ends a directory.
    async fn list(&self, prefix: &str) -> anyhow::Result<Vec<Object>> {
        let _permit = self.traffic.permit().await?;
        self.patiently(|| self.list_once(prefix)).await
    }

    async fn list_once(&self, prefix: &str) -> anyhow::Result<Vec<Object>> {
        match &self.place {
            Place::Dir(dir) => {
                self.traffic.operations.fetch_add(1, Ordering::Relaxed);
                let (dir, prefix) = (dir.join(prefix), prefix.to_owned());
                tokio::task::spawn_blocking(move || list_dir(&dir, &prefix)).await?
            }
            Place::S3(s3) => s3.list(prefix).await,
        }
    }

    /// Removes the object under `name`.
    async fn delete(&self, name: &str) -> anyhow::Result<()> {
        self.may_write()?;
        let _permit = self.traffic.permit().await?;
        self.patiently(|| self.delete_once(name)).await
    }

    async fn delete_once(&self, name: &str) -> anyhow::Result<()> {
        match &self.place {
            Place::Dir(dir) => {
                self.traffic.operations.fetch_add(1, Ordering::Relaxed);
                match tokio::fs::remove_file(dir.join(name)).await {
                    Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                        Err(e).with_context(|| format!("removing {name}"))
                    }
                    _ => Ok(()),
                }
            }
            Place::S3(s3) => s3.delete(name).await,
        }
    }

    /// Writes a small object, unless what is there is no longer what was read.
    async fn write_small(
        &self,
        name: &str,
        bytes: Vec<u8>,
        read_at: Option<&Version>,
    ) -> anyhow::Result<Written> {
        self.may_write()?;
        let _permit = self.traffic.permit().await?;
        let bytes = Arc::new(bytes);
        self.patiently(|| self.write_small_once(name, bytes.clone(), read_at)).await
    }

    /// A write whose answer was lost and that is made again finds its own bytes there and
    /// is told it lost, which has the caller read them and see there is nothing to add.
    async fn write_small_once(
        &self,
        name: &str,
        bytes: Arc<Vec<u8>>,
        read_at: Option<&Version>,
    ) -> anyhow::Result<Written> {
        match &self.place {
            Place::Dir(dir) => {
                self.traffic.operations.fetch_add(1, Ordering::Relaxed);
                let (to, read_at) = (dir.join(name), read_at.and_then(|it| it.0.clone()));
                let write = move || write_if_unchanged(&to, &bytes, read_at.as_deref());
                tokio::task::spawn_blocking(write).await?
            }
            Place::S3(s3) => s3.write_small(name, &bytes, read_at).await,
        }
    }

    /// Puts the file under `name`, all of it or nothing.
    async fn send_file(&self, name: &str, file: &Path, len: u64) -> anyhow::Result<()> {
        self.may_write()?;
        match &self.place {
            Place::Dir(dir) => {
                let _permit = self.traffic.permit().await?;
                self.traffic.operations.fetch_add(1, Ordering::Relaxed);
                let (from, to) = (file.to_owned(), dir.join(name));
                tokio::task::spawn_blocking(move || copy_whole(&from, &to)).await??;
                self.traffic.sent.fetch_add(len, Ordering::Relaxed);
                Ok(())
            }
            Place::S3(s3) => s3.send_file(name, file, len).await,
        }
    }

    /// The lines of the list `name`, for a machine that only reads it. No list is no lines.
    pub(crate) async fn read_list(&self, name: &str) -> anyhow::Result<Option<String>> {
        let Some((bytes, _)) = self.read_small(name).await? else { return Ok(None) };
        let text = String::from_utf8(bytes).with_context(|| format!("`{name}` is not text"))?;
        let opened = self.trust.open(name, &text).inspect_err(|_| {
            self.traffic.rejected.fetch_add(1, Ordering::Relaxed);
        });
        Ok(Some(opened?.to_owned()))
    }

    /// Replaces the lines of the list `name` by what `extend` makes of them, again if
    /// another machine wrote in between: no write undoes another.
    pub(crate) async fn extend_list(
        &self,
        name: &str,
        extend: impl Fn(&str) -> String,
    ) -> anyhow::Result<()> {
        self.rewrite_list(name, extend, true).await
    }

    /// As [`Self::extend_list`], for lines being taken out: a list this machine would not
    /// add to is left as it is, and that is an error.
    async fn shrink_list(&self, name: &str, shrink: impl Fn(&str) -> String) -> anyhow::Result<()> {
        self.rewrite_list(name, shrink, false).await
    }

    async fn rewrite_list(
        &self,
        name: &str,
        rewrite: impl Fn(&str) -> String,
        anew: bool,
    ) -> anyhow::Result<()> {
        for _ in 0..LIST_WRITES {
            let read = self.read_small(name).await?;
            let text = match &read {
                Some((bytes, _)) => std::str::from_utf8(bytes).unwrap_or_default(),
                None => "",
            };
            let lines = match self.trust.open_to_extend(name, text) {
                Ok(lines) => lines,
                Err(e) if read.is_none() => {
                    drop(e);
                    ""
                }
                // Signing it would be vouching for it.
                Err(e) if anew => {
                    eprintln!("remote: {e:#}: it is written anew");
                    ""
                }
                Err(e) => return Err(e),
            };
            let next = self.trust.seal(name, &rewrite(lines)).into_bytes();
            let read_at = read.as_ref().map(|it| &it.1);
            if let Written::Done = self.write_small(name, next, read_at).await? {
                return Ok(());
            }
        }
        bail!("`{name}` was written by others {LIST_WRITES} times in a row")
    }

    /// The bytes of a small object whose name is their hash. Bytes that hash to anything
    /// else are counted as rejected.
    async fn read_named_by_hash(&self, name: &str, want: &Digest) -> anyhow::Result<Vec<u8>> {
        let Some((bytes, _)) = self.read_small(name).await? else {
            return Err(transfer::Missing(name.to_owned()).into());
        };
        let got = Digest::of(&bytes);
        if got != *want {
            self.traffic.rejected.fetch_add(1, Ordering::Relaxed);
            bail!("rejected {name}: its {} bytes hash to {got}, not to {want}", bytes.len());
        }
        Ok(bytes)
    }

    /// Sends an object whose name is the hash of its bytes, unless it is there.
    pub(crate) async fn send_named_by_hash(
        &self,
        name: &str,
        bytes: Vec<u8>,
    ) -> anyhow::Result<()> {
        if !self.holds(name, bytes.len() as u64).await? {
            // Whoever else writes it writes the same bytes.
            self.write_small(name, bytes, None).await?;
        }
        Ok(())
    }

    /// How many requests are made of the remote at a time.
    pub(crate) fn jobs(&self) -> usize {
        self.traffic.jobs()
    }

    pub(crate) fn traffic(&self) -> &Traffic {
        &self.traffic
    }
}

/// The files of a directory of a directory remote, named as objects are. No directory is
/// no files.
fn list_dir(dir: &Path, prefix: &str) -> anyhow::Result<Vec<Object>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("listing {prefix}")),
    };
    let mut objects = Vec::new();
    for entry in entries {
        let entry = entry?;
        let found = entry.metadata()?;
        let Some(name) = entry.file_name().to_str().map(|it| format!("{prefix}{it}")) else {
            continue;
        };
        if found.is_file() {
            objects.push(Object { name, len: found.len(), written: found.modified()? });
        }
    }
    Ok(objects)
}

/// Copies a file into a directory remote so that it is there whole or not at all.
fn copy_whole(from: &Path, to: &Path) -> std::io::Result<()> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut partial = to.as_os_str().to_owned();
    partial.push(format!(".{}.tmp", std::process::id()));
    let partial = PathBuf::from(partial);
    let copied = std::fs::copy(from, &partial)
        .and_then(|_| File::options().write(true).open(&partial))
        .and_then(|file| name_file_whole(&file, &partial, to));
    if copied.is_err() {
        _ = std::fs::remove_file(&partial);
    }
    copied
}

/// Writes a file of a directory remote if it still hashes to `read_at`, under a lock that
/// the other machines writing it take too.
fn write_if_unchanged(to: &Path, bytes: &[u8], read_at: Option<&str>) -> anyhow::Result<Written> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut lock = to.as_os_str().to_owned();
    lock.push(".lock");
    let lock = File::create(lock)?;
    lock.lock()?;
    let now = match std::fs::read(to) {
        Ok(bytes) => Some(Digest::of(&bytes).to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.into()),
    };
    if now.as_deref() != read_at {
        return Ok(Written::Lost);
    }
    write_file_whole(to, bytes)?;
    Ok(Written::Done)
}

/// How the transfers of one push or pull ended.
#[derive(Default)]
pub(crate) struct Tally {
    pub(crate) done: usize,
    pub(crate) bytes: u64,
    /// Left to another process that was fetching the same thing.
    pub(crate) elsewhere: usize,
    pub(crate) failed: Vec<anyhow::Error>,
}

impl Tally {
    /// Runs `work` on every item, a few at a time. One that fails does not stop the others.
    pub(crate) async fn of<T, F>(
        items: impl IntoIterator<Item = T>,
        jobs: usize,
        work: impl FnMut(T) -> F,
    ) -> Tally
    where
        F: Future<Output = anyhow::Result<Fetched>>,
    {
        let mut tally = Tally::default();
        let mut results = stream::iter(items).map(work).buffer_unordered(jobs);
        while let Some(result) = results.next().await {
            match result {
                Ok(Fetched::Whole(bytes)) => {
                    tally.done += 1;
                    tally.bytes += bytes;
                }
                Ok(Fetched::Elsewhere) => tally.elsewhere += 1,
                Err(e) => tally.failed.push(e),
            }
        }
        tally
    }

    /// `said` when everything went through, and an error that says how much did not
    /// otherwise, after each failure on a line of its own.
    pub(crate) fn settle(self, said: String, traffic: &Traffic) -> anyhow::Result<String> {
        if self.failed.is_empty() {
            return Ok(said);
        }
        for failure in &self.failed {
            eprintln!("remote: {failure:#}");
        }
        let rejected = traffic.rejected();
        bail!("{said}; {} failed, {rejected} of them rejected", self.failed.len())
    }
}

/// A pack as a manifest lists it: `PACK SIZE KEYS FORMAT PACKED PACKED_SIZE`, the hash of
/// the pack, how many bytes it is, the hash of the list of its keys, the format of the
/// store it is of, and the hash and size of the object that holds it compressed. A line
/// without the last two is a pack that was sent as it is.
#[derive(Clone, Copy)]
struct Listed {
    size: u64,
    keys: Digest,
    format: u32,
    packed: Option<(Digest, u64)>,
}

impl Listed {
    /// The object the pack is in, and the hash of that object.
    fn object(&self, pack: &Digest) -> (String, Digest) {
        match self.packed {
            Some((packed, _)) => (format!("packs/{pack}.pack.zst"), packed),
            None => (pack_object(pack), *pack),
        }
    }
}

/// What a project's builds were served from.
#[derive(Default)]
struct Manifest(BTreeMap<Digest, Listed>);

impl Manifest {
    fn at(project: &str) -> String {
        format!("manifests/{project}")
    }

    /// The packs of the lines that are whole. A line that is not is one this version does
    /// not know how to read, and names nothing to fetch.
    fn parse(lines: &str) -> Manifest {
        let packs = lines.lines().filter_map(|line| {
            let mut fields = line.split(' ');
            let pack = Digest::parse(fields.next()?)?;
            let size = fields.next()?.parse().ok()?;
            let keys = Digest::parse(fields.next()?)?;
            let format = fields.next()?.parse().ok()?;
            let packed = match (fields.next(), fields.next()) {
                (Some(packed), Some(size)) => Some((Digest::parse(packed)?, size.parse().ok()?)),
                _ => None,
            };
            Some((pack, Listed { size, keys, format, packed }))
        });
        Manifest(packs.collect())
    }

    fn lines(&self) -> String {
        let line = |(pack, it): (&Digest, &Listed)| match it.packed {
            Some((packed, size)) => {
                format!("{pack} {} {} {} {packed} {size}\n", it.size, it.keys, it.format)
            }
            None => format!("{pack} {} {} {}\n", it.size, it.keys, it.format),
        };
        self.0.iter().map(line).collect()
    }

    async fn fetch(remote: &Remote, project: &str) -> anyhow::Result<Option<Manifest>> {
        let lines = remote.read_list(&Manifest::at(project)).await?;
        Ok(lines.map(|it| Manifest::parse(&it)))
    }

    /// The packs of the format this build reads that are not in `local`, and how many of
    /// another format were passed over.
    fn missing(&self, local: &BTreeSet<Digest>) -> (Vec<(Digest, Listed)>, usize) {
        let absent = self.0.iter().filter(|(pack, _)| !local.contains(pack));
        let (ours, others): (Vec<_>, Vec<_>) = absent
            .map(|(pack, listed)| (*pack, *listed))
            .partition(|it| it.1.format == STORE_FORMAT);
        (ours, others.len())
    }
}

/// Whether what a fetched file starts with is a pack of the format this build reads. A
/// manifest that lists a pack as of this format and is wrong about it is not believed.
fn is_pack_of_this_format(start: &[u8]) -> bool {
    pack_format(start) == Some(STORE_FORMAT)
}

fn pack_file(pack: &Digest) -> String {
    format!("{pack}.pack")
}

fn pack_object(pack: &Digest) -> String {
    format!("packs/{pack}.pack")
}

fn keys_object(keys: &Digest) -> String {
    format!("keys/{keys}")
}

/// The sealed packs of a store, by the hash their names say.
fn local_packs(store: &StoreDir) -> anyhow::Result<BTreeSet<Digest>> {
    let names = store.packs()?;
    Ok(names.iter().filter_map(|it| Digest::parse(it.strip_suffix(".pack")?)).collect())
}

/// The keys of a sealed pack as one object, if the pack is what its name says. A pack
/// that is not is one a disk or a stopped process spoiled, and is never sent.
fn keys_of(store: &Path, pack: Digest) -> anyhow::Result<(u64, Vec<u8>)> {
    let name = pack_file(&pack);
    let file = File::open(store.join(&name))?;
    // SAFETY: a sealed pack is never written to again.
    let bytes = unsafe { memmap2::Mmap::map(&file)? };
    match pack_keys(&name, &bytes) {
        Ok(keys) => Ok((bytes.len() as u64, keys.concat())),
        Err(why) => bail!("{name} of the store {why}: it is not sent"),
    }
}

/// Sends a pack and its keys, unless the remote has them, and says how to list it.
async fn send_pack(store: &Path, remote: &Remote, pack: Digest) -> anyhow::Result<(Listed, u64)> {
    let from = store.to_owned();
    let (size, keys) = tokio::task::spawn_blocking(move || keys_of(&from, pack)).await??;
    let keys_digest = Digest::of(&keys);
    remote.send_named_by_hash(&keys_object(&keys_digest), keys).await?;
    let (from, aside) = (store.join(pack_file(&pack)), store.join(format!("{pack}.pack.send.tmp")));
    let packing = aside.clone();
    let packed = tokio::task::spawn_blocking(move || pack_whole(&from, &packing)).await?;
    let sent = async {
        let (packed, packed_size) = packed?;
        let listed = Listed {
            size,
            keys: keys_digest,
            format: STORE_FORMAT,
            packed: Some((packed, packed_size)),
        };
        let (object, _) = listed.object(&pack);
        if remote.holds(&object, packed_size).await? {
            return Ok((listed, 0));
        }
        remote.send_file(&object, &aside, packed_size).await?;
        Ok((listed, packed_size))
    }
    .await;
    _ = tokio::fs::remove_file(&aside).await;
    sent
}

/// How hard packs are compressed for the remote: zstd's default, which makes a pack a
/// seventh of its size at a tenth of a second for 50 MB.
const PACKING: i32 = 3;

/// Compresses the file `from` into `to`, a temporary file of the store's, and says what
/// hash and size it has.
fn pack_whole(from: &Path, to: &Path) -> anyhow::Result<(Digest, u64)> {
    let mut packed = zstd::Encoder::new(File::create(to)?, PACKING)?;
    std::io::copy(&mut File::open(from)?, &mut packed)?;
    packed.finish()?.sync_all()?;
    // SAFETY: nothing else writes a file this process made under its own name.
    let bytes = unsafe { memmap2::Mmap::map(&File::open(to)?)? };
    Ok((Digest::of(&bytes), bytes.len() as u64))
}

/// Makes the directory a store of this build's format, and seals what ended daemons left
/// open in it when `adopt` says to.
async fn claim(store: &Path, adopt: bool) -> anyhow::Result<StoreDir> {
    let dir = StoreDir::at(store);
    let claimed = tokio::task::spawn_blocking(move || {
        dir.claim()?;
        if adopt {
            dir.adopt();
        }
        std::io::Result::Ok(dir)
    });
    claimed.await?.with_context(|| store.display().to_string())
}

/// Sends the sealed packs the project's manifest lacks, and lists the ones that arrived.
pub(crate) async fn push(store: &Path, remote: &Remote, project: &str) -> anyhow::Result<String> {
    remote.may_write()?;
    let local = local_packs(&claim(store, true).await?)?;
    let listed = match Manifest::fetch(remote, project).await {
        Ok(manifest) => manifest.unwrap_or_default(),
        // A list this machine does not trust is one it does not add to: see `extend_list`.
        Err(_) => Manifest::default(),
    };
    let missing: Vec<Digest> = local.into_iter().filter(|it| !listed.0.contains_key(it)).collect();
    let (mut sent, mut bytes, mut failed) = (Manifest::default(), 0, Vec::new());
    let mut sending = stream::iter(
        missing.iter().map(|&pack| async move { (pack, send_pack(store, remote, pack).await) }),
    )
    .buffer_unordered(remote.traffic.jobs());
    while let Some((pack, result)) = sending.next().await {
        match result {
            Ok((listed, size)) => {
                sent.0.insert(pack, listed);
                bytes += size;
            }
            Err(e) => failed.push(e),
        }
    }
    drop(sending);
    if !sent.0.is_empty() {
        let extend = |lines: &str| {
            let mut manifest = Manifest::parse(lines);
            manifest.0.extend(sent.0.iter().map(|(pack, listed)| (*pack, *listed)));
            manifest.lines()
        };
        remote.extend_list(&Manifest::at(project), extend).await?;
    }
    let said = format!("{} packs added to `{project}`, {bytes} bytes sent", sent.0.len());
    Tally { failed, ..Tally::default() }.settle(said, &remote.traffic)
}

/// Fetches a pack as its line of the manifest says it is held.
async fn fetch_pack(
    remote: &Remote,
    store: &Path,
    pack: &Digest,
    listed: &Listed,
) -> anyhow::Result<Fetched> {
    let (object, want) = listed.object(pack);
    let unpacked = listed.packed.map(|_| *pack);
    let to = store.join(pack_file(pack));
    remote.fetch_coded(&object, &want, unpacked, &to, is_pack_of_this_format).await
}

/// The packs of `missing` that fit in a store that may hold `limit` bytes, the first listed
/// first, and how many do not.
fn fitting(missing: Vec<(Digest, Listed)>, limit: Option<u64>) -> (Vec<(Digest, Listed)>, usize) {
    let mut room = limit.unwrap_or(u64::MAX);
    let mut fits = |listed: &Listed| {
        let fits = listed.size <= room;
        room -= if fits { listed.size } else { 0 };
        fits
    };
    let (fit, over): (Vec<_>, Vec<_>) = missing.into_iter().partition(|(_, listed)| fits(listed));
    (fit, over.len())
}

/// The packs of `missing` that have a key the store lacks, and how many have none. A store
/// makes one pack of many small ones, so a pack that is not there by name may be there by
/// what is in it. A list of keys that cannot be had leaves its pack to be fetched.
async fn lacking(
    remote: &Remote,
    dir: StoreDir,
    missing: Vec<(Digest, Listed)>,
) -> anyhow::Result<(Vec<(Digest, Listed)>, usize)> {
    let here = tokio::task::spawn_blocking(move || dir.keys()).await??;
    // A store with nothing in it lacks them all, and no list has to be fetched to know.
    if here.is_empty() || missing.is_empty() {
        return Ok((missing, 0));
    }
    let here = &here;
    let asked = stream::iter(missing.into_iter().map(|(pack, listed)| async move {
        let keys = remote.read_named_by_hash(&keys_object(&listed.keys), &listed.keys).await;
        if let Err(e) = &keys {
            eprintln!("remote: the keys of {pack}.pack are not known, so it is fetched: {e:#}");
        }
        let all_here = keys.is_ok_and(|keys| {
            !keys.is_empty()
                && keys
                    .chunks_exact(32)
                    .all(|key| <&[u8; 32]>::try_from(key).is_ok_and(|key| here.contains(key)))
        });
        ((pack, listed), all_here)
    }));
    let asked: Vec<_> = asked.buffered(remote.traffic.jobs()).collect().await;
    let (here_already, lacking): (Vec<_>, Vec<_>) = asked.into_iter().partition(|it| it.1);
    Ok((lacking.into_iter().map(|it| it.0).collect(), here_already.len()))
}

/// Removes the packs that have gone unused the longest until the store is under its limit,
/// which what was just fetched may have put it over.
async fn keep_under(dir: StoreDir, limit: Option<u64>) -> anyhow::Result<String> {
    let collected = tokio::task::spawn_blocking(move || dir.collect(limit)).await??;
    Ok(match collected.evicted {
        0 => String::new(),
        evicted => format!(", {evicted} packs unused the longest evicted to stay under the limit"),
    })
}

/// Fetches the packs of the project's manifest that are not in the local store, as far as
/// they fit under `RUSTD_CACHE_MAX`. A pack of another format than this build reads is left
/// where it is.
pub(crate) async fn pull(store: &Path, remote: &Remote, project: &str) -> anyhow::Result<String> {
    let limit = limit_from_env().map_err(anyhow::Error::msg)?;
    let dir = claim(store, false).await?;
    let local = local_packs(&dir)?;
    let Some(manifest) = Manifest::fetch(remote, project).await? else {
        return Ok(format!("`{project}` has no manifest on the remote: nothing fetched"));
    };
    let (missing, other_format) = manifest.missing(&local);
    let (missing, here_already) = lacking(remote, StoreDir::at(store), missing).await?;
    let (fit, over) = fitting(missing, limit);
    let fetch = |(pack, listed): (Digest, Listed)| async move {
        fetch_pack(remote, store, &pack, &listed).await
    };
    let tally = Tally::of(fit, remote.traffic.jobs(), fetch).await;
    let mut said = format!(
        "{} packs of `{project}` fetched, {} bytes",
        tally.done + tally.elsewhere,
        tally.bytes
    );
    if other_format != 0 {
        said += &format!(", {other_format} of another format left");
    }
    if here_already != 0 {
        said += &format!(", {here_already} that are in other packs of the store passed over");
    }
    if over != 0 {
        said += &format!(", {over} left that do not fit under RUSTD_CACHE_MAX");
    }
    if tally.done != 0 {
        said += &keep_under(dir, limit).await?;
    }
    tally.settle(said, &remote.traffic)
}

const USAGE: &str = "usage: rustd cache push|pull REMOTE [PROJECT]\n       \
                     rustd cache stats\n       \
                     rustd cache gc [--max SIZE]\n       \
                     rustd cache gc --remote REMOTE [--unlisted-after AGE] [--older-than AGE] \
                     [--dry-run]\n       \
                     rustd cache verify\n       \
                     rustd cache keygen FILE";

/// `rustd cache VERB`, on the store `RUSTD_CACHE` names.
pub(crate) fn command(mut args: impl Iterator<Item = String>) -> anyhow::Result<()> {
    let Some(verb) = args.next() else { bail!(USAGE) };
    if verb == "keygen" {
        return signed::keygen(Path::new(&args.next().context(USAGE)?));
    }
    let mut args = args.peekable();
    if verb == "gc" && args.next_if(|it| it == "--remote").is_some() {
        return collect_remote(args);
    }
    let store = PathBuf::from(std::env::var_os("RUSTD_CACHE").context("RUSTD_CACHE is not set")?);
    match verb.as_str() {
        "stats" => stats(&store),
        "gc" => collect(&store, args),
        "verify" => verify(&store),
        "push" | "pull" => transfer_command(&store, &verb, args),
        _ => bail!(USAGE),
    }
}

fn transfer_command(
    store: &Path,
    verb: &str,
    mut args: impl Iterator<Item = String>,
) -> anyhow::Result<()> {
    let Some(remote) = args.next() else { bail!(USAGE) };
    let project = project_name(args.next())?;
    let start = Instant::now();
    let done = runtime()?.block_on(async {
        let remote = Remote::open(&remote).await?;
        let done = match verb {
            "push" => push(store, &remote, &project).await,
            _ => pull(store, &remote, &project).await,
        };
        eprintln!("remote: {}", remote.traffic);
        done
    })?;
    println!("{done} in {:.1?}", start.elapsed());
    Ok(())
}

/// `rustd cache gc --remote REMOTE`: takes out of the remote what its lists no longer
/// name, and of its lists what is old or held twice. An age is a count and `s`, `m`, `h`
/// or `d`.
fn collect_remote(mut args: impl Iterator<Item = String>) -> anyhow::Result<()> {
    let Some(remote) = args.next() else { bail!(USAGE) };
    let mut ages = collect::Ages { unlisted: collect::Ages::UNLISTED, listed: None, dry: false };
    while let Some(flag) = args.next() {
        let mut age = || {
            let age = args.next().context(USAGE)?;
            collect::parse_age(&age).with_context(|| format!("`{age}` is not an age"))
        };
        match flag.as_str() {
            "--unlisted-after" => ages.unlisted = age()?,
            "--older-than" => ages.listed = Some(age()?),
            "--dry-run" => ages.dry = true,
            _ => bail!(USAGE),
        }
    }
    let start = Instant::now();
    let done = runtime()?.block_on(async {
        let remote = Remote::open(&remote).await?;
        let done = collect::collect(&remote, &ages).await;
        eprintln!("remote: {}", remote.traffic);
        done
    })?;
    println!("{done}\nin {:.1?}", start.elapsed());
    Ok(())
}

fn format_line(format: Option<u32>) -> String {
    match format {
        None => "format: none (the directory has no store in it)".to_owned(),
        Some(STORE_FORMAT) => format!("format: {STORE_FORMAT}"),
        Some(other) => format!("format: {other} (this rustd reads {STORE_FORMAT})"),
    }
}

fn seconds_since(at: SystemTime) -> u64 {
    at.elapsed().map_or(0, |it| it.as_secs())
}

/// `rustd cache stats`: what the directory holds, without reading any pack.
fn stats(store: &Path) -> anyhow::Result<()> {
    let stats = StoreDir::at(store).stats().with_context(|| store.display().to_string())?;
    println!("{}", format_line(stats.format));
    println!("packs: {}", stats.packs);
    println!("pack bytes: {}", stats.pack_bytes);
    println!("open packs: {}", stats.open);
    println!("open bytes: {}", stats.open_bytes);
    println!("temporary files: {}", stats.temporary);
    println!("temporary bytes: {}", stats.temporary_bytes);
    match limit_from_env().map_err(anyhow::Error::msg)? {
        Some(limit) => println!("limit bytes: {limit}"),
        None => println!("limit bytes: off"),
    }
    if let Some((first, last)) = stats.used {
        println!("least recently used: {} seconds ago", seconds_since(first));
        println!("most recently used: {} seconds ago", seconds_since(last));
    }
    Ok(())
}

/// `rustd cache gc [--max SIZE]`: collects garbage now, down to the size given or the one
/// `RUSTD_CACHE_MAX` names.
fn collect(store: &Path, mut args: impl Iterator<Item = String>) -> anyhow::Result<()> {
    let limit = match (args.next().as_deref(), args.next()) {
        (None, _) => limit_from_env().map_err(anyhow::Error::msg)?,
        (Some("--max"), Some(size)) => {
            Some(parse_size(&size).with_context(|| format!("`{size}` is not a size"))?)
        }
        _ => bail!(USAGE),
    };
    let dir = StoreDir::at(store);
    dir.claim()?;
    let collected = dir.collect(limit)?;
    if collected.busy {
        bail!("another process is collecting the store");
    }
    println!("evicted packs: {}", collected.evicted);
    println!("evicted bytes: {}", collected.evicted_bytes);
    println!("merged packs: {}", collected.merged);
    println!("sealed open packs: {}", collected.adopted);
    println!("removed temporary files: {}", collected.abandoned);
    println!("kept packs: {}", collected.kept);
    println!("kept bytes: {}", collected.kept_bytes);
    Ok(())
}

/// `rustd cache verify`: reads everything and fails if any of it is not what was written.
fn verify(store: &Path) -> anyhow::Result<()> {
    let verified = StoreDir::at(store).verify().with_context(|| store.display().to_string())?;
    println!("{}", format_line(verified.format));
    println!("packs: {}", verified.packs);
    println!("records: {}", verified.records);
    println!("bytes: {}", verified.bytes);
    println!("open packs: {}", verified.open);
    println!("torn bytes: {}", verified.torn_bytes);
    println!("corrupt packs: {}", verified.corrupt.len());
    for (name, why) in &verified.corrupt {
        println!("corrupt: {name} {why}");
    }
    if verified.format.is_some_and(|it| it != STORE_FORMAT) {
        bail!("the store is not of the format this rustd reads");
    }
    if !verified.corrupt.is_empty() {
        bail!("{} packs are not what was written", verified.corrupt.len());
    }
    Ok(())
}

/// The name of a project, `default` when none is given, as the name of an object.
pub(crate) fn project_name(given: Option<String>) -> anyhow::Result<String> {
    let project = given.unwrap_or_else(|| "default".to_owned());
    if project.is_empty() || project.contains(['/', '\\']) || project.starts_with('.') {
        bail!("`{project}` is not the name of a project");
    }
    Ok(project)
}

/// The runtime the transfers of this process run on. The compiler's threads are not its
/// workers: they wait on it from outside.
pub(crate) fn runtime() -> anyhow::Result<Runtime> {
    let mut runtime = tokio::runtime::Builder::new_multi_thread();
    Ok(runtime.worker_threads(2).thread_name("rustd-remote").enable_all().build()?)
}

/// The remote the daemon keeps its store in step with: `RUSTD_REMOTE`, and the project
/// `RUSTD_PROJECT` names in it.
pub(crate) struct Shared {
    store: PathBuf,
    remote: Arc<Remote>,
    project: String,
    runtime: Arc<Runtime>,
    /// Whether packs are left on the remote until a build asks for a key of theirs.
    lazy: bool,
    /// How often what the daemon compiled is sent while it runs, if on a schedule at all.
    push_every: Option<Duration>,
    asked: Arc<keeper::Asked>,
}

impl Shared {
    /// `None` without a store or a remote. A remote that cannot be reached is not an error
    /// to the build: it compiles what it would have fetched.
    pub(crate) fn from_env() -> Option<Shared> {
        let store = PathBuf::from(std::env::var_os("RUSTD_CACHE")?);
        let remote = std::env::var("RUSTD_REMOTE").ok()?;
        let opened = runtime().and_then(|runtime| {
            let project = project_name(std::env::var("RUSTD_PROJECT").ok())?;
            let push_every = keeper::push_every_from_env()?;
            let remote = Arc::new(runtime.block_on(Remote::open(&remote))?);
            let lazy = std::env::var_os("RUSTD_REMOTE_LAZY").is_some_and(|it| it != "0");
            let (runtime, asked) = (Arc::new(runtime), Arc::default());
            Ok(Shared { store, remote, project, runtime, lazy, push_every, asked })
        });
        opened.inspect_err(|e| eprintln!("remote: {e:#}")).ok()
    }

    /// Fetches what the project's manifest lists, or with `RUSTD_REMOTE_LAZY` only what
    /// tells which pack holds which key, leaving each pack to the first build that asks.
    /// From here on the store asks this remote for the keys it lacks.
    pub(crate) fn pull(&self) {
        // An outage during one build is over by the next, or is found again.
        self.remote.traffic.try_again();
        let start = Instant::now();
        let done = self.runtime.block_on(async {
            self.asked.see(self.manifest_now().await);
            self.pull_as_set().await
        });
        let asked = self.asked.clone();
        hir_ty::mir::codegen::fetch_missing_with(Box::new(move |key| asked.fetch(key)));
        self.report(done, start);
    }

    /// A pull the way the daemon is set to pull: of every pack, or of what says where the
    /// keys are.
    async fn pull_as_set(&self) -> anyhow::Result<String> {
        if !self.lazy {
            return pull(&self.store, &self.remote, &self.project).await;
        }
        let lazy = Lazy::load(self).await?;
        let said =
            format!("{} keys in {} packs to fetch from", lazy.holder.len(), lazy.packs.len());
        self.asked.fetch_from(lazy);
        Ok(said)
    }

    /// What the project's manifest hashes to now: `None` when there is none or it cannot
    /// be read.
    async fn manifest_now(&self) -> Option<Digest> {
        let read = self.remote.read_small(&Manifest::at(&self.project)).await;
        Some(Digest::of(&read.ok()??.0))
    }

    pub(crate) fn push(&self) {
        if self.remote.may_write().is_err() {
            return;
        }
        self.remote.traffic.try_again();
        let start = Instant::now();
        let done = self.runtime.block_on(push(&self.store, &self.remote, &self.project));
        self.report(done, start);
    }

    fn report(&self, done: anyhow::Result<String>, start: Instant) {
        match done {
            Ok(done) => eprintln!("remote: {done} in {:.1?}", start.elapsed()),
            Err(e) => eprintln!("remote: {e:#}"),
        }
        eprintln!("remote: {}", self.remote.traffic);
    }
}

/// The packs of a manifest that are not here yet, and which of them holds each key.
struct Lazy {
    store: PathBuf,
    remote: Arc<Remote>,
    runtime: Arc<Runtime>,
    /// Each pack and whether it arrived, once it was asked for.
    packs: Vec<(Digest, Listed, OnceLock<bool>)>,
    holder: FxHashMap<[u8; 32], u32>,
}

impl Lazy {
    /// Fetches the keys of every pack the manifest lists that the store lacks. They are
    /// kept in memory: the store's directory holds nothing that is not the store's.
    async fn load(shared: &Shared) -> anyhow::Result<Lazy> {
        let (store, remote) = (&shared.store, &shared.remote);
        let local = local_packs(&claim(store, false).await?)?;
        let manifest = Manifest::fetch(remote, &shared.project).await?.unwrap_or_default();
        let (missing, _) = manifest.missing(&local);
        let fetched = stream::iter(missing.into_iter().map(|(pack, listed)| async move {
            let keys = remote.read_named_by_hash(&keys_object(&listed.keys), &listed.keys).await?;
            anyhow::Ok((pack, listed, keys))
        }))
        .buffer_unordered(remote.traffic.jobs())
        .collect::<Vec<_>>()
        .await;
        let mut lazy = Lazy {
            store: store.clone(),
            remote: remote.clone(),
            runtime: shared.runtime.clone(),
            packs: Vec::new(),
            holder: FxHashMap::default(),
        };
        for fetched in fetched {
            match fetched {
                Ok((pack, listed, keys)) => lazy.add(pack, listed, &keys),
                Err(e) => eprintln!("remote: {e:#}"),
            }
        }
        Ok(lazy)
    }

    fn add(&mut self, pack: Digest, listed: Listed, keys: &[u8]) {
        let at = self.packs.len() as u32;
        self.packs.push((pack, listed, OnceLock::new()));
        for key in keys.chunks_exact(32) {
            self.holder.entry(key.try_into().expect("32 bytes")).or_insert(at);
        }
    }

    /// Whether the pack that holds `key` is in the store now. Called by the threads that
    /// compile, which wait here for the pack: the first to ask fetches it.
    fn fetch(&self, key: &[u8; 32]) -> bool {
        let Some(&at) = self.holder.get(key) else { return false };
        let (pack, listed, arrived) = &self.packs[at as usize];
        *arrived.get_or_init(|| {
            let fetched =
                self.runtime.block_on(fetch_pack(&self.remote, &self.store, pack, listed));
            match fetched {
                Ok(Fetched::Whole(bytes)) => {
                    eprintln!("remote: {pack}.pack fetched for a build, {bytes} bytes");
                    true
                }
                Ok(Fetched::Elsewhere) => false,
                Err(e) => {
                    eprintln!("remote: {e:#}");
                    false
                }
            }
        })
    }
}
