//! What a database keeps beside its Salsa storage, saved and loaded with it: the inputs are
//! found by file, source root and crate through these tables, which Salsa knows nothing of.

use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use triomphe::Arc;
use vfs::{AbsPathBuf, FileId};

use crate::{
    Crate, CratesMap, FileSourceRootInput, FileText, Files, IncludedFile, SourceRootId,
    SourceRootInput, input::UniqueCrateData,
};

/// The inputs are saved as their Salsa ids, which the loaded storage hands out again.
#[derive(Serialize, Deserialize)]
pub struct Tables {
    files: Vec<(FileId, FileText)>,
    source_roots: Vec<(SourceRootId, SourceRootInput)>,
    file_source_roots: Vec<(FileId, FileSourceRootInput)>,
    included: Vec<(AbsPathBuf, IncludedFile)>,
    included_sources: Vec<(AbsPathBuf, SourceRootId)>,
    /// Each crate, and whether it is told from another with the same root file by its data.
    crates: Vec<(Crate, bool)>,
}

impl Tables {
    pub fn of(files: &Files, crates: &CratesMap) -> Tables {
        fn pairs<K: Clone + Eq + std::hash::Hash, V: Copy, S: std::hash::BuildHasher + Clone>(
            map: &DashMap<K, V, S>,
        ) -> Vec<(K, V)> {
            map.iter().map(|it| (it.key().clone(), *it.value())).collect()
        }
        Tables {
            files: pairs(&files.files),
            source_roots: pairs(&files.source_roots),
            file_source_roots: pairs(&files.file_source_roots),
            included: pairs(&files.included),
            included_sources: pairs(&files.included_sources),
            crates: crates.0.iter().map(|it| (*it.value(), it.key().is_disambiguated())).collect(),
        }
    }

    /// The tables of a database whose storage was loaded, `db`.
    pub fn restore(self, db: &dyn salsa::Database) -> (Files, CratesMap) {
        let files = Files {
            files: Arc::new(self.files.into_iter().collect()),
            source_roots: Arc::new(self.source_roots.into_iter().collect()),
            file_source_roots: Arc::new(self.file_source_roots.into_iter().collect()),
            included: Arc::new(self.included.into_iter().collect()),
            included_sources: Arc::new(self.included_sources.into_iter().collect()),
            ..Files::default()
        };
        let crates = CratesMap::default();
        for (krate, disambiguated) in self.crates {
            crates.0.insert(UniqueCrateData::of(db, krate, disambiguated), krate);
        }
        (files, crates)
    }
}
