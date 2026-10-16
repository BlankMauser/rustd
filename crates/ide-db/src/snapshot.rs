//! Saving a database to bytes and loading it in another process.
//!
//! Salsa writes the inputs, interned values and query results that are marked `persist`, with
//! the ids and revisions they have. The tables the database keeps beside them and the paths
//! the files are known by come along, so that the loaded database finds every input it had
//! and a workspace loaded into it writes only what differs.

use std::mem::ManuallyDrop;

use base_db::{Nonce, salsa, snapshot::Tables};
use salsa::Durability;
use serde::{Deserialize, Serialize};
use triomphe::Arc;
use vfs::VfsPath;

use crate::{RootDatabase, executions};

/// What has to be the same where a snapshot is loaded as where it was saved.
///
/// Salsa numbers its ingredients in the order of their type names, so a snapshot is only
/// read by the program that wrote it.
#[derive(Serialize, Deserialize, PartialEq, Eq, Debug)]
struct Header {
    magic: String,
    program: String,
}

impl Header {
    fn current() -> Header {
        let program = std::env::current_exe()
            .and_then(|exe| {
                let meta = exe.metadata()?;
                Ok(format!("{} bytes, modified {:?}", meta.len(), meta.modified()?))
            })
            .unwrap_or_default();
        Header { magic: "rustd snapshot 1".to_owned(), program }
    }
}

#[derive(Debug)]
pub enum SnapshotError {
    /// Written by another build of the program.
    Stale,
    Malformed(postcard::Error),
}

impl std::fmt::Display for SnapshotError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SnapshotError::Stale => f.write_str("the snapshot is of another build of the program"),
            SnapshotError::Malformed(err) => write!(f, "the snapshot does not read: {err}"),
        }
    }
}

impl std::error::Error for SnapshotError {}

impl From<postcard::Error> for SnapshotError {
    fn from(err: postcard::Error) -> Self {
        SnapshotError::Malformed(err)
    }
}

impl RootDatabase {
    /// The database as bytes, with `paths`, the path of every file id in id order.
    pub fn save(&mut self, paths: &[VfsPath]) -> Result<Vec<u8>, SnapshotError> {
        let bytes = postcard::to_extend(&(Header::current(), paths), Vec::new())?;
        let tables = Tables::of(&self.files, &self.crates_map);
        let db: &mut dyn salsa::Database = self;
        let bytes = postcard::to_extend(&db.as_serialize(), bytes)?;
        Ok(postcard::to_extend(&tables, bytes)?)
    }

    /// The database [`RootDatabase::save`] wrote, and the paths of its files.
    pub fn load(bytes: &[u8]) -> Result<(RootDatabase, Vec<VfsPath>), SnapshotError> {
        let mut bytes = postcard::Deserializer::from_bytes(bytes);
        if Header::deserialize(&mut bytes)? != Header::current() {
            return Err(SnapshotError::Stale);
        }
        let paths = Vec::deserialize(&mut bytes)?;
        // Nothing is in the storage before it is read: Salsa puts every value where it was.
        let mut db = RootDatabase {
            storage: ManuallyDrop::new(salsa::Storage::new(executions::callback())),
            files: Default::default(),
            crates_map: Default::default(),
            nonce: Nonce::new(),
        };
        (&mut db as &mut dyn salsa::Database).deserialize(&mut bytes)?;
        let (files, crates) = Tables::deserialize(&mut bytes)?.restore(&db);
        (db.files, db.crates_map) = (Arc::new(files), Arc::new(crates));
        // The inputs that are not saved are those of a new database.
        hir::ProcMacros::init_default(&db, Durability::MEDIUM);
        hir::db::set_expand_proc_attr_macros(&mut db, false);
        Ok((db, paths))
    }
}
