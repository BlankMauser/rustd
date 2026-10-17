//! What cargo builds: the units of a build of one package, with the features and the profile
//! of each. `cargo metadata` has neither as a build of a single package has them.

use std::{
    collections::BTreeSet,
    path::Path,
    process::{Command, Stdio},
};

use anyhow::Context;
use cfg::{CfgAtom, CfgDiff};
use intern::{Symbol, sym};
use rustc_hash::{FxHashMap, FxHashSet};
use serde_derive::Deserialize;

/// Which build of a package the daemon stands in for, as the flags of `cargo build` say it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Selection {
    /// The name of the profile: `dev` unless `--release` or `--profile` says otherwise.
    pub(crate) profile: String,
    pub(crate) features: Vec<String>,
    pub(crate) no_default_features: bool,
    pub(crate) all_features: bool,
    /// The triple built for, when it is not the host.
    pub(crate) target: Option<String>,
    /// Whether tests, examples and benchmarks are built, and so what they depend on.
    pub(crate) dev_deps: bool,
}

impl Default for Selection {
    fn default() -> Selection {
        Selection {
            profile: "dev".to_owned(),
            features: Vec::new(),
            no_default_features: false,
            all_features: false,
            target: None,
            dev_deps: false,
        }
    }
}

/// How a package is built for the target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Built {
    pub(crate) features: BTreeSet<String>,
    pub(crate) profile: Profile,
}

/// The settings of a profile that change what a program does.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub(crate) struct Profile {
    pub(crate) name: String,
    pub(crate) debug_assertions: bool,
    pub(crate) overflow_checks: bool,
    pub(crate) panic: PanicStrategy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum PanicStrategy {
    Unwind,
    Abort,
    ImmediateAbort,
}

impl PanicStrategy {
    fn name(self) -> &'static str {
        match self {
            PanicStrategy::Unwind => "unwind",
            PanicStrategy::Abort | PanicStrategy::ImmediateAbort => "abort",
        }
    }
}

impl Profile {
    /// What the profile changes of the cfg `rustc --print cfg -O` reports.
    pub(crate) fn cfg(&self) -> CfgDiff {
        let flag = |name: Symbol| CfgAtom::Flag(name);
        let panic = |strategy: PanicStrategy| CfgAtom::KeyValue {
            key: sym::panic,
            value: Symbol::intern(strategy.name()),
        };
        let (mut enable, mut disable) = (Vec::new(), Vec::new());
        let mut set =
            |on: bool, atom: CfgAtom| if on { enable.push(atom) } else { disable.push(atom) };
        set(self.debug_assertions, flag(sym::debug_assertions));
        set(self.overflow_checks, flag(sym::overflow_checks));
        for strategy in [PanicStrategy::Unwind, PanicStrategy::Abort] {
            set(strategy.name() == self.panic.name(), panic(strategy));
        }
        CfgDiff::new(enable, disable)
    }
}

/// The packages of a build by their id, each as it is built for the target.
pub(crate) struct Plan {
    packages: FxHashMap<String, Built>,
}

impl Plan {
    /// Asks cargo for the units of the build `selection` names of the package of `manifest`.
    pub(crate) fn of(manifest: &Path, selection: &Selection) -> anyhow::Result<Plan> {
        let mut command = Command::new("cargo");
        // The unit graph is the only place cargo says which features and which profile each
        // package of one build has, and it is not stable yet.
        command.env("RUSTC_BOOTSTRAP", "1");
        command.args(["build", "--unit-graph", "-Z", "unstable-options"]);
        command.arg("--manifest-path").arg(manifest);
        command.arg("--profile").arg(&selection.profile);
        if let Some(target) = &selection.target {
            command.arg("--target").arg(target);
        }
        if !selection.features.is_empty() {
            command.arg("--features").arg(selection.features.join(","));
        }
        if selection.no_default_features {
            command.arg("--no-default-features");
        }
        if selection.all_features {
            command.arg("--all-features");
        }
        if selection.dev_deps {
            command.arg("--all-targets");
        }
        let output = command.stdin(Stdio::null()).output().context("running `cargo build`")?;
        anyhow::ensure!(
            output.status.success(),
            "`cargo build --unit-graph` failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
        let graph: UnitGraph =
            serde_json::from_slice(&output.stdout).context("reading cargo's unit graph")?;
        Ok(graph.plan())
    }

    /// How the package cargo calls `id` is built, when the build has it.
    pub(crate) fn package(&self, id: &str) -> Option<&Built> {
        self.packages.get(id)
    }

    /// The profiles of the packages, for telling one build from another.
    pub(crate) fn profiles(&self) -> impl Iterator<Item = (&str, &Profile)> {
        self.packages.iter().map(|(id, built)| (id.as_str(), &built.profile))
    }
}

#[derive(Deserialize)]
struct UnitGraph {
    units: Vec<Unit>,
    roots: Vec<usize>,
}

#[derive(Deserialize)]
struct Unit {
    pkg_id: String,
    target: UnitTarget,
    profile: Profile,
    features: Vec<String>,
    dependencies: Vec<UnitDependency>,
}

#[derive(Deserialize)]
struct UnitTarget {
    kind: Vec<String>,
}

#[derive(Deserialize)]
struct UnitDependency {
    index: usize,
}

impl Unit {
    /// Whether the unit runs while building: a build script or a proc macro. What it depends
    /// on is built for the host, with features and a profile of its own.
    fn runs_on_host(&self) -> bool {
        self.target.kind.iter().any(|it| it == "custom-build" || it == "proc-macro")
    }

    fn is_library(&self) -> bool {
        self.target.kind.iter().any(|it| it.ends_with("lib"))
    }
}

impl UnitGraph {
    /// The units that end up in the program: those the roots reach without going through one
    /// that runs on the host.
    fn for_target(&self) -> FxHashSet<usize> {
        let mut reached = FxHashSet::default();
        let mut queue: Vec<usize> =
            self.roots.iter().copied().filter(|&it| it < self.units.len()).collect();
        while let Some(index) = queue.pop() {
            let unit = &self.units[index];
            if unit.runs_on_host() || !reached.insert(index) {
                continue;
            }
            queue.extend(
                unit.dependencies.iter().map(|it| it.index).filter(|&it| it < self.units.len()),
            );
        }
        reached
    }

    fn plan(self) -> Plan {
        let for_target = self.for_target();
        // A package that is only built for the host, a proc macro, is as that build has it.
        let rank = |index: usize, unit: &Unit| (for_target.contains(&index), unit.is_library());
        let mut best: FxHashMap<&str, (usize, &Unit)> = FxHashMap::default();
        for (index, unit) in self.units.iter().enumerate() {
            let chosen = best.entry(unit.pkg_id.as_str()).or_insert((index, unit));
            if rank(index, unit) > rank(chosen.0, chosen.1) {
                *chosen = (index, unit);
            }
        }
        let packages = best
            .into_iter()
            .map(|(id, (_, unit))| {
                let features = unit.features.iter().cloned().collect();
                (id.to_owned(), Built { features, profile: unit.profile.clone() })
            })
            .collect();
        Plan { packages }
    }
}
