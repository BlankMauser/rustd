//! Keeping a store in step with a place other machines reach: `rustd cache push REMOTE` and
//! `rustd cache pull REMOTE`, and the same done by the daemon when `RUSTD_REMOTE` names one.
//! The commands on the local store alone, `rustd cache stats|gc|verify`, are here with them.
//!
//! A remote is a directory or `s3://bucket/prefix`. It holds sealed packs under the hash of
//! their bytes, the keys of each pack under the hash of those, and one manifest a project:
//! the packs its builds were served from. A push sends the packs the manifest lacks and
//! adds them to it; a pull fetches the ones it lists that are not here. With
//! `RUSTD_REMOTE_LAZY` the daemon fetches the keys only, and a pack when a build first asks
//! for a key it holds.
//!
//! What is fetched becomes a pack of the store like any other: it is written the way the
//! store writes its own, counts toward `RUSTD_CACHE_MAX`, and can be evicted and fetched
//! again. A pull fetches no more than fits under that limit.
//!
//! Nothing fetched is used before its bytes hash to the name it was listed under, so a
//! remote can withhold code and cannot change it. What it could change is the list, which
//! is what `signed.rs` is for. `RUSTD_REMOTE_READONLY` keeps a machine from writing at all.
//!
//! Nor does a remote say how much is taken from it. A list is read to [`LIST_MAX`] bytes,
//! a pack to the size its list gives it, anything else to `RUSTD_REMOTE_MAX`, and what is
//! longer is refused with nothing of it kept.

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
use signed::{Trust, Turns, Unremembered};
use transfer::{Body, TooLong, Version};
pub(crate) use transfer::{Digest, Fetched, Room, Traffic};

/// How often a list is read again and written again after another machine wrote first.
const LIST_WRITES: usize = 8;

/// The most that is read of a list. A line of one is under 200 bytes, so this is a list
/// of some hundred thousand packs or units.
pub(crate) const LIST_MAX: u64 = 16 << 20;

/// The most that is taken of one object that is neither a list nor a pack, unless
/// `RUSTD_REMOTE_MAX` says: the archive of a unit, and the keys of the packs a daemon
/// fetches from when asked.
const OBJECT_MAX: u64 = 512 << 20;

pub(crate) struct Remote {
    place: Place,
    traffic: Arc<Traffic>,
    /// How long a request may go without an answer, or a body without a byte.
    patience: Duration,
    read_only: bool,
    trust: Trust,
    turns: Turns,
    /// `RUSTD_REMOTE_MAX`, or [`OBJECT_MAX`].
    object_max: u64,
}

enum Place {
    /// A directory, such as a mount shared by the machines.
    Dir(PathBuf),
    S3(Box<S3>),
}

impl Remote {
    /// The remote `spec` names, as the environment has this machine use it:
    /// `RUSTD_REMOTE_JOBS` requests at a time (8), `RUSTD_REMOTE_TIMEOUT` seconds without
    /// an answer (30), `RUSTD_REMOTE_READONLY` to never write, `RUSTD_REMOTE_MAX` bytes of
    /// one object, and the keys of `signed.rs`. The turns of its lists are remembered in
    /// `kept_in`, the directory that what is fetched goes to.
    pub(crate) async fn open(spec: &str, kept_in: &Path) -> anyhow::Result<Remote> {
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
        let object_max = match std::env::var("RUSTD_REMOTE_MAX") {
            Ok(text) => parse_size(&text)
                .with_context(|| format!("RUSTD_REMOTE_MAX=`{text}` is not a size"))?,
            Err(_) => OBJECT_MAX,
        };
        let turns = Turns::of(kept_in, &place.name());
        Ok(Remote {
            place,
            traffic,
            patience,
            read_only,
            trust: Trust::from_env()?,
            turns,
            object_max,
        })
    }

    /// The most that is taken of one object that says nothing of its size.
    pub(crate) fn object_max(&self) -> u64 {
        self.object_max
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
    /// long as a pack is. One of more than `most` bytes is an error, and no more than a
    /// read past that many of them is held.
    async fn read_small(
        &self,
        name: &str,
        most: u64,
    ) -> anyhow::Result<Option<(Vec<u8>, Version)>> {
        let _permit = self.traffic.permit().await?;
        self.patiently(|| self.read_small_once(name, most)).await
    }

    async fn read_small_once(
        &self,
        name: &str,
        most: u64,
    ) -> anyhow::Result<Option<(Vec<u8>, Version)>> {
        match &self.place {
            Place::Dir(dir) => {
                self.traffic.operations.fetch_add(1, Ordering::Relaxed);
                let file = match tokio::fs::File::open(dir.join(name)).await {
                    Ok(file) => file,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                    Err(e) => return Err(e).with_context(|| format!("fetching {name}")),
                };
                let bytes = read_at_most(Body::File(file), most, name).await?;
                let version = Version(Some(Digest::of(&bytes).to_string()));
                Ok(Some((bytes, version)))
            }
            Place::S3(s3) => s3.read_small(name, most).await,
        }
    }

    /// How many bytes the object under `name` has, if there is one.
    async fn size(&self, name: &str) -> anyhow::Result<Option<u64>> {
        let _permit = self.traffic.permit().await?;
        self.patiently(|| self.size_once(name)).await
    }

    async fn size_once(&self, name: &str) -> anyhow::Result<Option<u64>> {
        match &self.place {
            Place::Dir(dir) => {
                self.traffic.operations.fetch_add(1, Ordering::Relaxed);
                match tokio::fs::metadata(dir.join(name)).await {
                    Ok(found) => Ok(Some(found.len())),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                    Err(e) => Err(e).with_context(|| format!("looking for {name}")),
                }
            }
            Place::S3(s3) => s3.size(name).await,
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

    /// The lines of the list `name`, for a machine that takes what it names. No list is
    /// no lines. A signed list older than one this machine took before is an error.
    pub(crate) async fn read_list(&self, name: &str) -> anyhow::Result<Option<String>> {
        let Some((bytes, _)) = self.read_small(name, LIST_MAX).await? else { return Ok(None) };
        let text = String::from_utf8(bytes).with_context(|| format!("`{name}` is not text"))?;
        let taken = match self.trust.open(name, &text) {
            Ok(opened) => self.take_turn(name, opened.turn).await.map(|()| opened.lines),
            Err(e) => Err(e),
        };
        let lines = taken.inspect_err(|_| {
            self.traffic.rejected.fetch_add(1, Ordering::Relaxed);
        });
        Ok(Some(lines?.to_owned()))
    }

    /// The lines of the list `name`, for a machine that is about to add to it: what it
    /// would write the list back with. A list it would not sign under is an error.
    pub(crate) async fn read_list_to_extend(&self, name: &str) -> anyhow::Result<Option<String>> {
        let Some((bytes, _)) = self.read_small(name, LIST_MAX).await? else { return Ok(None) };
        let text = String::from_utf8(bytes).with_context(|| format!("`{name}` is not text"))?;
        Ok(Some(self.trust.open_to_extend(name, &text)?.lines.to_owned()))
    }

    /// Remembers the turn a signed list was taken at, unless a later one was taken before.
    async fn take_turn(&self, name: &str, turn: Option<u64>) -> anyhow::Result<()> {
        let Some(turn) = turn else { return Ok(()) };
        let (turns, name) = (self.turns.clone(), name.to_owned());
        tokio::task::spawn_blocking(move || turns.take(&name, turn)).await?
    }

    /// The highest turn this machine took of the list `name`, or 0.
    async fn turn_taken(&self, name: &str) -> anyhow::Result<u64> {
        let (turns, name) = (self.turns.clone(), name.to_owned());
        let taken = tokio::task::spawn_blocking(move || turns.remembered(&name)).await??;
        Ok(taken.unwrap_or(0))
    }

    /// Forgets the turns this machine took of the lists of this remote.
    pub(crate) fn forget_turns(&self) -> std::io::Result<bool> {
        self.turns.forget()
    }

    /// Replaces the lines of the list `name` by what `extend` makes of them, again if
    /// another machine wrote in between: no write undoes another.
    pub(crate) async fn extend_list(
        &self,
        name: &str,
        extend: impl Fn(&str) -> String,
    ) -> anyhow::Result<()> {
        for _ in 0..LIST_WRITES {
            let read = self.read_small(name, LIST_MAX).await?;
            let text = match &read {
                Some((bytes, _)) => std::str::from_utf8(bytes).unwrap_or_default(),
                None => "",
            };
            let opened = match self.trust.open_to_extend(name, text) {
                Ok(opened) => self.take_turn(name, opened.turn).await.map(|()| opened.lines),
                Err(e) => Err(e),
            };
            let lines = match opened {
                Ok(lines) => lines,
                Err(e) if e.is::<Unremembered>() => return Err(e),
                Err(_) if read.is_none() => "",
                // Signing it would be vouching for it.
                Err(e) => {
                    eprintln!("remote: {e:#}: it is written anew");
                    ""
                }
            };
            // The list it replaces was at the turn remembered, or was not taken at all.
            let after = self.turn_taken(name).await?;
            let (next, turn) = self.trust.seal(name, &extend(lines), after);
            let read_at = read.as_ref().map(|it| &it.1);
            if let Written::Done = self.write_small(name, next.into_bytes(), read_at).await? {
                return self.take_turn(name, turn).await;
            }
        }
        bail!("`{name}` was written by others {LIST_WRITES} times in a row")
    }

    /// The bytes of a small object whose name is their hash, when they are at most `most`.
    /// Bytes that hash to anything else are counted as rejected.
    async fn read_named_by_hash(
        &self,
        name: &str,
        want: &Digest,
        most: u64,
    ) -> anyhow::Result<Vec<u8>> {
        let Some((bytes, _)) = self.read_small(name, most).await? else {
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
        if self.size(name).await? != Some(bytes.len() as u64) {
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

impl Place {
    /// What tells this remote from another on this machine: where it is, and not how it
    /// is reached.
    fn name(&self) -> String {
        match self {
            Place::Dir(dir) => dir.display().to_string(),
            Place::S3(s3) => s3.place(),
        }
    }
}

/// All the bytes of `body` when they are at most `most`: an error when there are more,
/// with at most one read past that many of them held.
async fn read_at_most(mut body: Body, most: u64, name: &str) -> anyhow::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    while let Some(chunk) = body.next().await? {
        if (bytes.len() + chunk.len()) as u64 > most {
            return Err(TooLong { name: name.to_owned(), most }.into());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
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

/// A pack as a manifest lists it: `PACK SIZE KEYS FORMAT`, the hash of the pack, how many
/// bytes it is, the hash of the list of its keys, and the format of the store it is of.
#[derive(Clone, Copy)]
struct Listed {
    size: u64,
    keys: Digest,
    format: u32,
}

impl Listed {
    /// The most the list of the pack's keys can be: the keys are in the pack too.
    fn keys_max(&self, remote: &Remote) -> u64 {
        self.size.min(remote.object_max)
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
            Some((pack, Listed { size, keys, format }))
        });
        Manifest(packs.collect())
    }

    fn lines(&self) -> String {
        let line = |(pack, it): (&Digest, &Listed)| {
            format!("{pack} {} {} {}\n", it.size, it.keys, it.format)
        };
        self.0.iter().map(line).collect()
    }

    async fn fetch(remote: &Remote, project: &str) -> anyhow::Result<Option<Manifest>> {
        let lines = remote.read_list(&Manifest::at(project)).await?;
        Ok(lines.map(|it| Manifest::parse(&it)))
    }

    /// The manifest as the machine that adds to it reads it.
    async fn fetch_to_extend(remote: &Remote, project: &str) -> anyhow::Result<Option<Manifest>> {
        let lines = remote.read_list_to_extend(&Manifest::at(project)).await?;
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
    let listed = Listed { size, keys: Digest::of(&keys), format: STORE_FORMAT };
    remote.send_named_by_hash(&keys_object(&listed.keys), keys).await?;
    if remote.size(&pack_object(&pack)).await? == Some(size) {
        return Ok((listed, 0));
    }
    remote.send_file(&pack_object(&pack), &store.join(pack_file(&pack)), size).await?;
    Ok((listed, size))
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
    let listed = match Manifest::fetch_to_extend(remote, project).await {
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

/// The packs of `missing` that fit in a store that may hold `limit` bytes, the first listed
/// first, and how many do not.
fn fitting(missing: Vec<(Digest, Listed)>, limit: Option<u64>) -> (Vec<(Digest, u64)>, usize) {
    let mut room = limit.unwrap_or(u64::MAX);
    let mut fits = |listed: &Listed| {
        let fits = listed.size <= room;
        room -= if fits { listed.size } else { 0 };
        fits
    };
    let (fit, over): (Vec<_>, Vec<_>) = missing.into_iter().partition(|(_, listed)| fits(listed));
    (fit.into_iter().map(|(pack, listed)| (pack, listed.size)).collect(), over.len())
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
        let most = listed.keys_max(remote);
        let keys = remote.read_named_by_hash(&keys_object(&listed.keys), &listed.keys, most).await;
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
/// where it is, and one that is longer than the manifest says is rejected.
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
    // A pack is as long as its list says, so what is written is what was found to fit.
    let fetch = |(pack, size): (Digest, u64)| {
        let (from, to) = (pack_object(&pack), store.join(pack_file(&pack)));
        async move { remote.fetch_file(&from, &pack, &to, size, is_pack_of_this_format).await }
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
                     rustd cache verify\n       \
                     rustd cache keygen FILE\n       \
                     rustd cache forget REMOTE";

/// `rustd cache VERB`, on the store `RUSTD_CACHE` names.
pub(crate) fn command(mut args: impl Iterator<Item = String>) -> anyhow::Result<()> {
    let Some(verb) = args.next() else { bail!(USAGE) };
    if verb == "keygen" {
        return signed::keygen(Path::new(&args.next().context(USAGE)?));
    }
    let store = PathBuf::from(std::env::var_os("RUSTD_CACHE").context("RUSTD_CACHE is not set")?);
    match verb.as_str() {
        "stats" => stats(&store),
        "gc" => collect(&store, args),
        "verify" => verify(&store),
        "push" | "pull" => transfer_command(&store, &verb, args),
        "forget" => forget(&store, args),
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
        let remote = Remote::open(&remote, store).await?;
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

/// `rustd cache forget REMOTE`: forgets the turns taken of the remote's lists, so that
/// whatever list it has next is taken. For a remote that was started again from nothing
/// by a machine whose clock is behind, or a memory that was damaged.
fn forget(store: &Path, mut args: impl Iterator<Item = String>) -> anyhow::Result<()> {
    let Some(remote) = args.next() else { bail!(USAGE) };
    let forgot = runtime()?
        .block_on(async { anyhow::Ok(Remote::open(&remote, store).await?.forget_turns()?) })?;
    if forgot {
        println!("the turns taken of the lists of {remote} are forgotten");
    } else {
        println!("no turn was taken of a list of {remote}");
    }
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
}

impl Shared {
    /// `None` without a store or a remote. A remote that cannot be reached is not an error
    /// to the build: it compiles what it would have fetched.
    pub(crate) fn from_env() -> Option<Shared> {
        let store = PathBuf::from(std::env::var_os("RUSTD_CACHE")?);
        let remote = std::env::var("RUSTD_REMOTE").ok()?;
        let opened = runtime().and_then(|runtime| {
            let project = project_name(std::env::var("RUSTD_PROJECT").ok())?;
            let remote = Arc::new(runtime.block_on(Remote::open(&remote, &store))?);
            Ok(Shared { store, remote, project, runtime: Arc::new(runtime) })
        });
        opened.inspect_err(|e| eprintln!("remote: {e:#}")).ok()
    }

    /// Fetches what the project's manifest lists, or with `RUSTD_REMOTE_LAZY` only what
    /// tells which pack holds which key, leaving each pack to the first build that asks.
    pub(crate) fn pull(&self) {
        // An outage during one build is over by the next, or is found again.
        self.remote.traffic.try_again();
        let start = Instant::now();
        let lazy = std::env::var_os("RUSTD_REMOTE_LAZY").is_some_and(|it| it != "0");
        let done = self.runtime.block_on(async {
            if lazy {
                let lazy = Lazy::load(self).await?;
                let said = format!(
                    "{} keys in {} packs to fetch from",
                    lazy.holder.len(),
                    lazy.packs.len()
                );
                hir_ty::mir::codegen::fetch_missing_with(Box::new(move |key| lazy.fetch(key)));
                Ok(said)
            } else {
                pull(&self.store, &self.remote, &self.project).await
            }
        });
        self.report(done, start);
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
    /// Each pack, how long its list says it is, and whether it arrived, once it was asked
    /// for.
    packs: Vec<(Digest, u64, OnceLock<bool>)>,
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
        // A pack the store may not hold is not one to fetch, and the keys of them all are
        // kept in memory: no more of them than one object may be.
        let limit = limit_from_env().map_err(anyhow::Error::msg)?.unwrap_or(u64::MAX);
        let held = &Room::of(Some(remote.object_max));
        let fetched = stream::iter(missing.into_iter().map(|(pack, listed)| async move {
            if listed.size > limit {
                bail!("{pack}.pack is listed as {} bytes, over RUSTD_CACHE_MAX", listed.size);
            }
            let most = listed.keys_max(remote);
            let keys =
                remote.read_named_by_hash(&keys_object(&listed.keys), &listed.keys, most).await?;
            held.take(keys.len() as u64)?;
            Ok((pack, listed.size, keys))
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
                Ok((pack, size, keys)) => lazy.add(pack, size, &keys),
                Err(e) => eprintln!("remote: {e:#}"),
            }
        }
        Ok(lazy)
    }

    fn add(&mut self, pack: Digest, size: u64, keys: &[u8]) {
        let at = self.packs.len() as u32;
        self.packs.push((pack, size, OnceLock::new()));
        for key in keys.chunks_exact(32) {
            self.holder.entry(key.try_into().expect("32 bytes")).or_insert(at);
        }
    }

    /// Whether the pack that holds `key` is in the store now. Called by the threads that
    /// compile, which wait here for the pack: the first to ask fetches it.
    fn fetch(&self, key: &[u8; 32]) -> bool {
        let Some(&at) = self.holder.get(key) else { return false };
        let (pack, size, arrived) = &self.packs[at as usize];
        *arrived.get_or_init(|| {
            let to = self.store.join(pack_file(pack));
            let fetched = self.runtime.block_on(self.remote.fetch_file(
                &pack_object(pack),
                pack,
                &to,
                *size,
                is_pack_of_this_format,
            ));
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

#[cfg(test)]
impl Remote {
    /// The directory `dir` as a remote, with what is taken of its lists remembered in
    /// `kept_in`.
    pub(crate) fn in_dir(dir: &Path, kept_in: &Path, trust: Trust, object_max: u64) -> Remote {
        let place = Place::Dir(dir.to_owned());
        let turns = Turns::of(kept_in, &place.name());
        Remote {
            place,
            traffic: Arc::new(Traffic::new(8)),
            patience: Duration::from_secs(5),
            read_only: false,
            trust,
            turns,
            object_max,
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rustd-remote-{name}-{}", std::process::id()));
        _ = std::fs::remove_dir_all(&dir);
        dir
    }

    /// Writes an object of a directory remote.
    pub(crate) fn put(remote: &Path, name: &str, bytes: &[u8]) {
        let to = remote.join(name);
        std::fs::create_dir_all(to.parent().unwrap()).unwrap();
        std::fs::write(to, bytes).unwrap();
    }

    /// The keys of a machine that names none and was told to take what is there.
    pub(crate) fn taking_unsigned() -> Trust {
        Trust::of(None, &[], true)
    }

    /// A remote in a new directory whose lists nobody signs and this machine takes.
    pub(crate) fn unsigned(name: &str, object_max: u64) -> (Remote, PathBuf) {
        let dir = scratch(name);
        (Remote::in_dir(&dir, &dir.join("kept"), taking_unsigned(), object_max), dir)
    }

    pub(crate) fn wait<T>(work: impl Future<Output = T>) -> T {
        runtime().unwrap().block_on(work)
    }

    /// `seed` and `runs` changes of it, the same ones in every run: bytes cut, flipped,
    /// put in and taken out.
    pub(crate) fn changed(seed: &[u8], runs: usize) -> impl Iterator<Item = Vec<u8>> {
        let mut state = 0x9e37_79b9_7f4a_7c15_u64;
        let mut next = move |bound: usize| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state % bound.max(1) as u64) as usize
        };
        let seed = seed.to_vec();
        std::iter::once(seed.clone()).chain((0..runs).map(move |_| {
            let mut bytes = seed.clone();
            for _ in 0..=next(4) {
                let at = next(bytes.len());
                match next(5) {
                    0 => bytes.truncate(at),
                    1 if !bytes.is_empty() => bytes[at] ^= 1 << next(8),
                    2 => bytes.insert(at, b"\n /.\\\0\xff"[next(7)]),
                    3 if !bytes.is_empty() => _ = bytes.remove(at),
                    _ => {
                        let from = next(bytes.len());
                        let again = bytes[from..].to_vec();
                        bytes.splice(at..at, again);
                    }
                }
            }
            bytes
        }))
    }

    /// How many bytes the files below `dir` hold.
    pub(crate) fn held(dir: &Path) -> u64 {
        let Ok(entries) = std::fs::read_dir(dir) else { return 0 };
        entries
            .flatten()
            .map(|it| match it.metadata() {
                Ok(meta) if meta.is_dir() => held(&it.path()),
                Ok(meta) => meta.len(),
                Err(_) => 0,
            })
            .sum()
    }

    #[test]
    fn an_object_is_read_to_the_bound_and_not_past_it() {
        let (remote, dir) = unsigned("bounded", 1 << 20);
        put(&dir, "keys/long", &[7; 100]);
        wait(async {
            for most in [100, 1 << 40] {
                assert_eq!(
                    remote.read_small("keys/long", most).await.unwrap().unwrap().0.len(),
                    100
                );
            }
            for most in [0, 1, 99] {
                let refused = remote.read_small("keys/long", most).await.err().unwrap();
                assert!(refused.is::<TooLong>(), "{refused}");
            }
            assert!(remote.read_small("keys/none", 0).await.unwrap().is_none());
        });
        _ = std::fs::remove_dir_all(&dir);
    }

    /// An object with no end is given up on at the bound.
    #[cfg(unix)]
    #[test]
    fn an_object_without_an_end_is_given_up_on() {
        wait(async {
            let endless = tokio::fs::File::open("/dev/zero").await.unwrap();
            let refused = read_at_most(Body::File(endless), 1 << 20, "zero").await.unwrap_err();
            assert!(refused.is::<TooLong>(), "{refused}");
        });
    }

    /// A remote with one thing that starts as a pack does, listed as `listed_size` bytes.
    fn remote_with_pack(name: &str, len: usize, listed_size: u64) -> (Remote, PathBuf, Digest) {
        let (remote, dir) = unsigned(name, 1 << 20);
        let mut pack = [&b"rustdpk"[..], &[STORE_FORMAT as u8]].concat();
        pack.resize(len, 7);
        let (named, keys) = (Digest::of(&pack), Digest::of(&[1; 32]));
        put(&dir, &pack_object(&named), &pack);
        put(&dir, &keys_object(&keys), &[1; 32]);
        let listed = Listed { size: listed_size, keys, format: STORE_FORMAT };
        put(&dir, "manifests/default", Manifest([(named, listed)].into()).lines().as_bytes());
        (remote, dir, named)
    }

    #[test]
    fn a_pack_longer_than_its_list_says_is_not_written() {
        let len = 3 * (1 << 16) + 100;
        for listed_size in [0, 8, 1 << 16, len as u64 - 1] {
            let (remote, dir, _) = remote_with_pack("long-pack", len, listed_size);
            let store = dir.join("store");
            let refused = wait(pull(&store, &remote, "default")).unwrap_err().to_string();
            assert!(refused.contains("1 failed, 1 of them rejected"), "{refused}");
            let left: Vec<_> = std::fs::read_dir(&store).unwrap().flatten().collect();
            let left: Vec<_> = left.iter().map(|it| it.file_name()).collect();
            assert!(left.iter().all(|it| it == "version" || it == "lock"), "{left:?}");
            _ = std::fs::remove_dir_all(&dir);
        }
        let (remote, dir, named) = remote_with_pack("exact-pack", len, len as u64);
        let store = dir.join("store");
        std::fs::create_dir_all(&store).unwrap();
        assert!(matches!(wait(fetched(&remote, &store, named, len as u64)), Fetched::Whole(_)));
        assert_eq!(held(&store), len as u64);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let pack = std::fs::metadata(store.join(pack_file(&named))).unwrap();
            assert_eq!(pack.permissions().mode() & 0o777, 0o600, "a fetched pack is the user's");
        }
        _ = std::fs::remove_dir_all(&dir);
    }

    async fn fetched(remote: &Remote, store: &Path, pack: Digest, most: u64) -> Fetched {
        let (from, to) = (pack_object(&pack), store.join(pack_file(&pack)));
        remote.fetch_file(&from, &pack, &to, most, is_pack_of_this_format).await.unwrap()
    }

    #[test]
    fn what_a_stopped_fetch_left_does_not_count_against_the_bound_twice() {
        let (remote, dir, named) = remote_with_pack("resumed-pack", 1000, 1000);
        let store = dir.join("store");
        std::fs::create_dir_all(&store).unwrap();
        // Longer than the pack may be, and not its start.
        std::fs::write(store.join(format!("{}.fetch.tmp", pack_file(&named))), [9; 5000]).unwrap();
        assert!(matches!(wait(fetched(&remote, &store, named, 1000)), Fetched::Whole(1000)));
        assert_eq!(held(&store), 1000);
        _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_manifest_longer_than_is_read_is_refused() {
        let (remote, dir) = unsigned("long-manifest", 1 << 20);
        let line = format!("{0} 4096 {0} {STORE_FORMAT}\n", "ab".repeat(32));
        let mut list = line.repeat(LIST_MAX as usize / line.len());
        list.push_str(&"\n".repeat(LIST_MAX as usize - list.len()));
        put(&dir, "manifests/default", list.as_bytes());
        wait(async {
            assert_eq!(Manifest::fetch(&remote, "default").await.unwrap().unwrap().0.len(), 1);
            list.push('\n');
            put(&dir, "manifests/default", list.as_bytes());
            let refused = Manifest::fetch(&remote, "default").await.err().unwrap();
            assert!(refused.is::<TooLong>(), "{refused}");
            assert!(remote.read_list_to_extend("manifests/default").await.is_err());
            let written = remote.extend_list("manifests/default", |lines| lines.to_owned()).await;
            assert!(written.is_err());
        });
        assert_eq!(std::fs::read(dir.join("manifests/default")).unwrap().len(), list.len());
        _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_keys_of_a_pack_are_no_longer_than_the_pack() {
        let (remote, dir) = unsigned("long-keys", 1 << 10);
        let listed = |size| Listed { size, keys: Digest::of(b""), format: STORE_FORMAT };
        assert_eq!(listed(100).keys_max(&remote), 100);
        assert_eq!(listed(u64::MAX).keys_max(&remote), 1 << 10);
        _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_bytes_break_the_reading_of_a_manifest() {
        let (first, second) = ("ab".repeat(32), "0f".repeat(32));
        let seed = format!("{first} 4096 {second} 2\n{second} 18446744073709551615 {first} 2\n");
        let mut listed = 0;
        for bytes in changed(seed.as_bytes(), 20_000) {
            let Ok(text) = String::from_utf8(bytes) else { continue };
            let manifest = Manifest::parse(&text);
            for (pack, it) in &manifest.0 {
                // What a pack is fetched under and into is a name of digits and nothing else.
                let name = pack_file(pack);
                assert!(name.len() == 69 && !name[..64].contains(['/', '\\', '\0', '.']), "{name}");
                assert_eq!(keys_object(&it.keys).len(), 69);
                listed += 1;
            }
            let (fit, over) = fitting(manifest.0.into_iter().collect(), Some(1 << 20));
            assert!(fit.iter().map(|it| it.1).sum::<u64>() <= 1 << 20 && fit.len() + over <= 4);
        }
        assert!(listed > 1000);
    }

    /// Two machines and a remote between them: one signs, the other trusts its key.
    struct Signed {
        dir: PathBuf,
        signer: Remote,
        reader: Remote,
    }

    impl Signed {
        fn new(name: &str) -> Signed {
            let dir = scratch(name);
            let at = dir.join("remote");
            let signer =
                Remote::in_dir(&at, &dir.join("a"), Trust::of(Some(1), &[], false), 1 << 20);
            let reader = Remote::in_dir(&at, &dir.join("b"), Trust::of(None, &[1], false), 1 << 20);
            Signed { dir, signer, reader }
        }

        fn list(&self) -> Vec<u8> {
            std::fs::read(self.dir.join("remote/manifests/default")).unwrap()
        }

        fn put_back(&self, list: &[u8]) {
            put(&self.dir.join("remote"), "manifests/default", list);
        }

        async fn add(&self, line: &'static str) {
            let add = move |lines: &str| format!("{lines}{line}\n");
            self.signer.extend_list("manifests/default", add).await.unwrap();
        }

        async fn read(&self) -> anyhow::Result<String> {
            Ok(self.reader.read_list("manifests/default").await?.unwrap_or_default())
        }
    }

    #[test]
    fn a_list_that_was_put_back_is_not_taken() {
        let signed = Signed::new("put-back");
        wait(async {
            signed.add("one").await;
            let first = signed.list();
            assert_eq!(signed.read().await.unwrap(), "one\n");
            signed.add("two").await;
            let second = signed.list();
            assert_eq!(signed.read().await.unwrap(), "one\ntwo\n");
            // The remote, or whoever writes to it, puts the first list back.
            signed.put_back(&first);
            let refused = signed.read().await.unwrap_err().to_string();
            assert!(refused.contains("an older list is not taken"), "{refused}");
            assert_eq!(signed.reader.traffic.rejected(), 1);
            // And is taken at its word again once it has the list it had.
            signed.put_back(&second);
            assert_eq!(signed.read().await.unwrap(), "one\ntwo\n");
            // A machine that has taken nothing takes the old list: it has no way to know.
            let new = dir_reader(&signed, "c");
            signed.put_back(&first);
            assert_eq!(new.read_list("manifests/default").await.unwrap().unwrap(), "one\n");
            // Forgetting is how a machine is told to take whatever is there.
            assert!(signed.reader.forget_turns().unwrap());
            assert_eq!(signed.read().await.unwrap(), "one\n");
        });
        _ = std::fs::remove_dir_all(&signed.dir);
    }

    fn dir_reader(signed: &Signed, name: &str) -> Remote {
        let trust = Trust::of(None, &[1], false);
        Remote::in_dir(&signed.dir.join("remote"), &signed.dir.join(name), trust, 1 << 20)
    }

    #[test]
    fn a_signer_does_not_add_to_a_list_that_was_put_back() {
        let signed = Signed::new("put-back-under-a-signer");
        let turn = |list: &[u8]| -> u64 {
            let text = std::str::from_utf8(list).unwrap();
            text.lines().last().unwrap().split(' ').nth(2).unwrap().parse().unwrap()
        };
        wait(async {
            signed.add("one").await;
            let first = signed.list();
            signed.add("two").await;
            let second = signed.list();
            assert!(turn(&second) > turn(&first));
            signed.put_back(&first);
            signed.add("three").await;
            // What it signs now names what it adds and comes after all it signed before.
            let third = signed.list();
            assert!(turn(&third) > turn(&second), "{} after {}", turn(&third), turn(&second));
            assert_eq!(signed.read().await.unwrap(), "three\n");
            // A reader that had the second list takes the third and not the second again.
            signed.put_back(&second);
            assert!(signed.read().await.is_err());
        });
        _ = std::fs::remove_dir_all(&signed.dir);
    }

    #[test]
    fn a_memory_that_is_damaged_stops_a_push_and_a_pull() {
        let signed = Signed::new("damaged-memory");
        wait(async {
            signed.add("one").await;
            assert_eq!(signed.read().await.unwrap(), "one\n");
            let list = signed.list();
            for kept in ["a", "b"] {
                let turns = signed.dir.join(kept).join("turns");
                let of_remote = std::fs::read_dir(turns).unwrap().next().unwrap().unwrap().path();
                for file in std::fs::read_dir(of_remote).unwrap().flatten() {
                    if file.file_name() != "lock" {
                        std::fs::write(file.path(), "not a turn").unwrap();
                    }
                }
            }
            let add = |lines: &str| format!("{lines}two\n");
            let refused = signed.signer.extend_list("manifests/default", add).await.unwrap_err();
            assert!(refused.is::<Unremembered>(), "{refused}");
            assert!(signed.read().await.unwrap_err().is::<Unremembered>());
            assert_eq!(signed.list(), list, "the list is as it was");
        });
        _ = std::fs::remove_dir_all(&signed.dir);
    }
}
