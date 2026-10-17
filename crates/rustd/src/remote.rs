//! Sending the packs of the local store to a place other machines read them from, and
//! fetching theirs: `rustd cache push REMOTE` and `rustd cache pull REMOTE`. The commands on
//! the local store alone, `rustd cache stats|gc|verify`, are here with them.
//!
//! A remote holds packs under the names they have locally, which are the hashes of their
//! bytes, and a manifest per project that lists the packs a build of it found useful. A
//! machine that starts with nothing fetches the manifest and then the packs, a few large
//! files, and checks each against its name.

use std::{
    collections::BTreeSet,
    io::Read,
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, SystemTime},
};

use anyhow::{Context, bail};
use hir_ty::mir::codegen::{
    STORE_FORMAT, StoreDir, limit_from_env, pack_name, parse_size, write_file_whole,
};
use rusty_s3::{Bucket, Credentials, S3Action, UrlStyle};

/// How long a signed request is good for: one pack on a slow line.
const SIGNED_FOR: Duration = Duration::from_secs(60 * 60);
const TRANSFERS: usize = 8;

pub(crate) enum Remote {
    /// A directory, such as a mount shared by the machines.
    Dir(PathBuf),
    S3 {
        bucket: Bucket,
        credentials: Credentials,
        /// With a `/` at its end, or empty.
        prefix: String,
    },
}

impl Remote {
    /// `s3://bucket/prefix`, with the endpoint, region and credentials as the AWS tools take
    /// them from the environment, or the path of a directory.
    pub(crate) fn parse(spec: &str) -> anyhow::Result<Remote> {
        let Some(rest) = spec.strip_prefix("s3://") else {
            return Ok(Remote::Dir(PathBuf::from(spec)));
        };
        let (name, prefix) = rest.split_once('/').unwrap_or((rest, ""));
        let prefix = match prefix.trim_end_matches('/') {
            "" => String::new(),
            prefix => format!("{prefix}/"),
        };
        let region = std::env::var("AWS_REGION")
            .or_else(|_| std::env::var("AWS_DEFAULT_REGION"))
            .unwrap_or_else(|_| "us-east-1".to_owned());
        // Path style is what the servers that are not AWS take.
        let (endpoint, style) = match std::env::var("AWS_ENDPOINT_URL") {
            Ok(endpoint) => (endpoint, UrlStyle::Path),
            Err(_) => (format!("https://s3.{region}.amazonaws.com"), UrlStyle::VirtualHost),
        };
        let endpoint = endpoint.parse().context("AWS_ENDPOINT_URL is not a URL")?;
        let bucket = Bucket::new(endpoint, style, name.to_owned(), region)?;
        let credentials = Credentials::from_env()
            .context("AWS_ACCESS_KEY_ID and AWS_SECRET_ACCESS_KEY are not set")?;
        Ok(Remote::S3 { bucket, credentials, prefix })
    }

    /// The bytes under `name`, or `None` when there is nothing under it.
    pub(crate) fn fetch(&self, name: &str) -> anyhow::Result<Option<Vec<u8>>> {
        match self {
            Remote::Dir(dir) => match std::fs::read(dir.join(name)) {
                Ok(bytes) => Ok(Some(bytes)),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(e.into()),
            },
            Remote::S3 { bucket, credentials, prefix } => {
                let key = format!("{prefix}{name}");
                let url = bucket.get_object(Some(credentials), &key).sign(SIGNED_FOR);
                match ureq::get(url.as_str()).call() {
                    Ok(response) => {
                        let mut bytes = Vec::new();
                        response.into_body().into_reader().read_to_end(&mut bytes)?;
                        Ok(Some(bytes))
                    }
                    Err(ureq::Error::StatusCode(404)) => Ok(None),
                    Err(e) => Err(e).with_context(|| format!("fetching {name}")),
                }
            }
        }
    }

    pub(crate) fn has(&self, name: &str) -> anyhow::Result<bool> {
        match self {
            Remote::Dir(dir) => Ok(dir.join(name).exists()),
            Remote::S3 { bucket, credentials, prefix } => {
                let key = format!("{prefix}{name}");
                let url = bucket.head_object(Some(credentials), &key).sign(SIGNED_FOR);
                match ureq::head(url.as_str()).call() {
                    Ok(_) => Ok(true),
                    Err(ureq::Error::StatusCode(404)) => Ok(false),
                    Err(e) => Err(e).with_context(|| format!("looking for {name}")),
                }
            }
        }
    }

    /// Puts `bytes` under `name`, all of them or none.
    pub(crate) fn send(&self, name: &str, bytes: &[u8]) -> anyhow::Result<()> {
        match self {
            Remote::Dir(dir) => {
                let to = dir.join(name);
                if let Some(parent) = to.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                Ok(write_file_whole(&to, bytes)?)
            }
            Remote::S3 { bucket, credentials, prefix } => {
                let key = format!("{prefix}{name}");
                let url = bucket.put_object(Some(credentials), &key).sign(SIGNED_FOR);
                ureq::put(url.as_str()).send(bytes).with_context(|| format!("sending {name}"))?;
                Ok(())
            }
        }
    }
}

/// Whether this machine takes what a remote lists. Nothing on a remote says who wrote a
/// list, and whoever writes one decides what the programs built from it run: a pack is
/// checked against its name, which is the hash of whatever its writer put in it. So a list
/// is taken only where `RUSTD_REMOTE_UNSIGNED=1` says that everyone who can write to the
/// remote is trusted as the user is. Sending to a remote asks for nothing.
pub(crate) struct Unsigned;

impl Unsigned {
    const SET: &str = "RUSTD_REMOTE_UNSIGNED";

    /// An error unless the environment says lists nobody signed are taken.
    pub(crate) fn taken() -> anyhow::Result<()> {
        Self::taken_when(std::env::var(Self::SET).ok().as_deref())
    }

    fn taken_when(set: Option<&str>) -> anyhow::Result<()> {
        if set == Some("1") {
            return Ok(());
        }
        bail!(
            "nothing is fetched from a remote whose lists are not signed: whoever can write \
             to it decides what the build runs. Set {}=1 if all who can are trusted",
            Self::SET
        )
    }
}

/// What a project's builds were served from: the names of packs, one a line.
struct Manifest(BTreeSet<String>);

impl Manifest {
    fn at(project: &str) -> String {
        format!("manifests/{project}")
    }

    fn fetch(remote: &Remote, project: &str) -> anyhow::Result<Manifest> {
        let bytes = remote.fetch(&Manifest::at(project))?.unwrap_or_default();
        Manifest::parse(bytes)
    }

    /// The packs the bytes of a manifest list. A line that is not the name of a pack is
    /// not one, and is never the name of a file.
    fn parse(bytes: Vec<u8>) -> anyhow::Result<Manifest> {
        let text = String::from_utf8(bytes).context("the manifest is not text")?;
        Ok(Manifest(
            text.lines()
                .filter(|it| hir_ty::mir::codegen::is_pack(it))
                .map(str::to_owned)
                .collect(),
        ))
    }

    fn send(&self, remote: &Remote, project: &str) -> anyhow::Result<()> {
        let text: String = self.0.iter().flat_map(|it| [it.as_str(), "\n"]).collect();
        remote.send(&Manifest::at(project), text.as_bytes())
    }
}

fn local_packs(store: &StoreDir) -> anyhow::Result<BTreeSet<String>> {
    Ok(store.packs()?.into_iter().collect())
}

/// Runs `work` on each name, a few at a time, and returns how many bytes went by.
pub(crate) fn transfer(
    names: &[&String],
    work: impl Fn(&str) -> anyhow::Result<u64> + Sync,
) -> anyhow::Result<u64> {
    let next = Mutex::new(names.iter());
    let total = Mutex::new(Ok(0));
    std::thread::scope(|scope| {
        for _ in 0..TRANSFERS.min(names.len()) {
            scope.spawn(|| {
                while let Some(name) = next.lock().ok().and_then(|mut it| it.next()) {
                    let done = work(name);
                    let Ok(mut total) = total.lock() else { break };
                    match (&mut *total, done) {
                        (Ok(total), Ok(bytes)) => *total += bytes,
                        (Ok(_), Err(e)) => *total = Err(e),
                        (Err(_), _) => break,
                    }
                }
            });
        }
    });
    total.into_inner().unwrap_or_else(|_| bail!("a transfer panicked"))
}

/// Sends the sealed packs the remote lacks and lists all local ones in the project's manifest.
pub(crate) fn push(store: &Path, remote: &Remote, project: &str) -> anyhow::Result<String> {
    let dir = StoreDir::at(store);
    dir.claim()?;
    dir.adopt();
    let local = local_packs(&dir)?;
    let mut manifest = Manifest::fetch(remote, project)?;
    let missing: Vec<&String> = local.iter().filter(|it| !manifest.0.contains(*it)).collect();
    let sent = transfer(&missing, |name| {
        let at = format!("packs/{name}");
        if remote.has(&at)? {
            return Ok(0);
        }
        let bytes = std::fs::read(store.join(name))?;
        // One that was damaged on disk is not handed to other machines.
        if pack_name(&bytes).as_deref() != Some(name) {
            bail!("{name} is not what was sealed under that name");
        }
        remote.send(&at, &bytes)?;
        Ok(bytes.len() as u64)
    })?;
    let listed = missing.len();
    manifest.0.extend(local);
    manifest.send(remote, project)?;
    Ok(format!("{listed} packs added to `{project}`, {} MB sent", sent >> 20))
}

/// Fetches the packs of the project's manifest that are not in the local store. A pack of
/// another format than this build reads is left where it is.
pub(crate) fn pull(store: &Path, remote: &Remote, project: &str) -> anyhow::Result<String> {
    Unsigned::taken()?;
    let dir = StoreDir::at(store);
    dir.claim()?;
    let local = local_packs(&dir)?;
    let manifest = Manifest::fetch(remote, project)?;
    let missing: Vec<&String> = manifest.0.difference(&local).collect();
    let other_format = AtomicUsize::new(0);
    let fetched = transfer(&missing, |name| {
        let Some(bytes) = remote.fetch(&format!("packs/{name}"))? else {
            bail!("`{project}` lists {name}, which is not there");
        };
        match pack_name(&bytes) {
            Some(named) if named == name => {}
            Some(_) => bail!("{name} is not what was sealed under that name"),
            None => {
                other_format.fetch_add(1, Ordering::Relaxed);
                return Ok(0);
            }
        }
        dir.write_whole(name, &bytes)?;
        Ok(bytes.len() as u64)
    })?;
    let skipped = match other_format.into_inner() {
        0 => String::new(),
        skipped => format!(", {skipped} of another format left"),
    };
    Ok(format!("{} packs of `{project}` fetched, {} MB{skipped}", missing.len(), fetched >> 20))
}

const USAGE: &str = "usage: rustd cache push|pull REMOTE [PROJECT]\n       \
                     rustd cache stats\n       \
                     rustd cache gc [--max SIZE]\n       \
                     rustd cache verify";

/// `rustd cache VERB`, on the store `RUSTD_CACHE` names.
pub(crate) fn command(mut args: impl Iterator<Item = String>) -> anyhow::Result<()> {
    let store = PathBuf::from(std::env::var_os("RUSTD_CACHE").context("RUSTD_CACHE is not set")?);
    let Some(verb) = args.next() else { bail!(USAGE) };
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
    let project = args.next().unwrap_or_else(|| "default".to_owned());
    if project.contains(['/', '\\']) || project.starts_with('.') {
        bail!("`{project}` is not the name of a project");
    }
    let remote = Remote::parse(&remote)?;
    let start = std::time::Instant::now();
    let done = match verb {
        "push" => push(store, &remote, &project)?,
        _ => pull(store, &remote, &project)?,
    };
    println!("{done} in {:.1?}", start.elapsed());
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

/// The remote the daemon keeps its store in step with: `RUSTD_REMOTE`, and the project
/// `RUSTD_PROJECT` names in it.
pub(crate) struct Shared {
    store: PathBuf,
    remote: Remote,
    project: String,
}

impl Shared {
    /// `None` without a store or a remote. A remote that cannot be reached is not an error
    /// to the build: it compiles what it would have fetched.
    pub(crate) fn from_env() -> Option<Shared> {
        let store = PathBuf::from(std::env::var_os("RUSTD_CACHE")?);
        let remote = std::env::var("RUSTD_REMOTE").ok()?;
        let project = std::env::var("RUSTD_PROJECT").unwrap_or_else(|_| "default".to_owned());
        match Remote::parse(&remote) {
            Ok(remote) => Some(Shared { store, remote, project }),
            Err(e) => {
                eprintln!("remote: {e:#}");
                None
            }
        }
    }

    pub(crate) fn pull(&self) {
        let start = std::time::Instant::now();
        match pull(&self.store, &self.remote, &self.project) {
            Ok(done) => eprintln!("remote: {done} in {:.1?}", start.elapsed()),
            Err(e) => eprintln!("remote: {e:#}"),
        }
    }

    pub(crate) fn push(&self) {
        let start = std::time::Instant::now();
        match push(&self.store, &self.remote, &self.project) {
            Ok(done) => eprintln!("remote: {done} in {:.1?}", start.elapsed()),
            Err(e) => eprintln!("remote: {e:#}"),
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

    #[test]
    fn lists_nobody_signed_are_taken_only_when_asked_for() {
        assert!(Unsigned::taken_when(Some("1")).is_ok());
        for unset in [None, Some(""), Some("0"), Some("true"), Some("yes")] {
            let refused = Unsigned::taken_when(unset).unwrap_err().to_string();
            assert!(refused.contains("RUSTD_REMOTE_UNSIGNED=1"), "{refused}");
        }
    }

    #[test]
    fn no_bytes_break_the_reading_of_a_manifest() {
        let seed = format!("{0}.pack\n{1}.pack\n", "ab".repeat(32), "0f".repeat(32));
        let mut listed = 0;
        for bytes in changed(seed.as_bytes(), 20_000) {
            let Ok(manifest) = Manifest::parse(bytes) else { continue };
            for name in &manifest.0 {
                assert!(name.len() == 69 && name.ends_with(".pack"), "{name}");
                assert!(!name[..64].contains(['/', '\\', '\0', '.']), "{name}");
                listed += 1;
            }
        }
        assert!(listed > 1000);
    }
}
