//! Machine code kept on disk between runs of the daemon, by what it was compiled from.
//!
//! The key is Cranelift's hash of a function's IR and the target, so a hit is code for exactly
//! that IR: nothing here knows about Rust. A value is what Cranelift serializes of the compiled
//! function. Building the IR is not saved, only compiling it.
//!
//! The store is a directory of packs, each a run of `key, length, value` records that one
//! daemon appended. Packs are only ever added, and a record cut short by a crash ends its pack.

use std::{
    borrow::Cow,
    fs::File,
    io::{BufWriter, Write},
    path::Path,
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicUsize, Ordering},
    },
};

use cranelift_codegen::incremental_cache::CacheKvStore;
use rustc_hash::{FxHashMap, FxHashSet};

pub(super) const KEY: usize = 32;

pub(super) struct Store {
    packs: Vec<Vec<u8>>,
    /// Where the value of a key is: the pack, and the range in it.
    index: FxHashMap<[u8; KEY], (u32, u32, u32)>,
    new: Mutex<New>,
    hits: AtomicUsize,
    misses: AtomicUsize,
}

/// The pack this process appends to. What is in it is found by the next run, not this one.
struct New {
    pack: BufWriter<File>,
    keys: FxHashSet<[u8; KEY]>,
}

impl Store {
    /// The store of this process: the directory `RUSTD_CACHE` names, when it is set.
    pub(super) fn global() -> Option<&'static Store> {
        static STORE: OnceLock<Option<Store>> = OnceLock::new();
        STORE
            .get_or_init(|| {
                let dir = std::env::var_os("RUSTD_CACHE")?;
                Store::open(Path::new(&dir)).ok()
            })
            .as_ref()
    }

    fn open(dir: &Path) -> std::io::Result<Store> {
        std::fs::create_dir_all(dir)?;
        let mut packs = Vec::new();
        let mut index = FxHashMap::default();
        for entry in std::fs::read_dir(dir)? {
            let path = entry?.path();
            if path.extension().is_none_or(|it| it != "pack") {
                continue;
            }
            let pack = std::fs::read(&path)?;
            let mut at = 0;
            while let Some(header) = pack.get(at..at + KEY + 4) {
                let (key, len) = header.split_at(KEY);
                let len = u32::from_le_bytes(len.try_into().expect("four bytes")) as usize;
                let start = at + KEY + 4;
                if pack.len() < start + len {
                    break;
                }
                let key: [u8; KEY] = key.try_into().expect("a key");
                index.insert(key, (packs.len() as u32, start as u32, len as u32));
                at = start + len;
            }
            packs.push(pack);
        }
        let name = format!(
            "{}-{}.pack",
            std::process::id(),
            std::time::UNIX_EPOCH.elapsed().map_or(0, |it| it.as_nanos())
        );
        let pack = BufWriter::new(File::create(dir.join(name))?);
        let new = Mutex::new(New { pack, keys: FxHashSet::default() });
        Ok(Store { packs, index, new, hits: AtomicUsize::new(0), misses: AtomicUsize::new(0) })
    }

    /// Writes out what has been added. What is not written by the time the daemon ends is
    /// compiled again by the next.
    pub(super) fn flush(&self) {
        if let Ok(mut new) = self.new.lock() {
            _ = new.pack.flush();
        }
    }

    pub(super) fn get(&self, key: &[u8; KEY]) -> Option<&[u8]> {
        let &(pack, start, len) = self.index.get(key)?;
        let (start, len) = (start as usize, len as usize);
        Some(&self.packs[pack as usize][start..start + len])
    }

    /// Adds a value, unless the key has one.
    pub(super) fn insert(&self, key: &[u8; KEY], value: &[u8]) {
        if self.index.contains_key(key) {
            return;
        }
        let (Ok(mut new), Ok(len)) = (self.new.lock(), u32::try_from(value.len())) else {
            return;
        };
        if !new.keys.insert(*key) {
            return;
        }
        // One write a record: a pack cut short loses its last record and no other.
        let mut record = Vec::with_capacity(KEY + 4 + value.len());
        record.extend_from_slice(key);
        record.extend_from_slice(&len.to_le_bytes());
        record.extend_from_slice(value);
        _ = new.pack.write_all(&record);
    }

    /// How many functions were found, and how many were not, since the last call.
    pub(super) fn take_counts(&self) -> (usize, usize) {
        (self.hits.swap(0, Ordering::Relaxed), self.misses.swap(0, Ordering::Relaxed))
    }
}

/// The store as Cranelift asks for it: one of these per function compiled.
pub(super) struct Lookup(pub(super) &'static Store);

impl CacheKvStore for Lookup {
    fn get(&self, key: &[u8]) -> Option<Cow<'_, [u8]>> {
        let found = <&[u8; KEY]>::try_from(key).ok().and_then(|key| self.0.get(key));
        let counted = if found.is_some() { &self.0.hits } else { &self.0.misses };
        counted.fetch_add(1, Ordering::Relaxed);
        found.map(Cow::Borrowed)
    }

    fn insert(&mut self, key: &[u8], val: Vec<u8>) {
        if let Ok(key) = <&[u8; KEY]>::try_from(key) {
            self.0.insert(key, &val);
        }
    }
}
