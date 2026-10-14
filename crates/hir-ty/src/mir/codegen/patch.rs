//! Answering an edit without writing the whole object again: the object of the last full build
//! stays as it is, and a small one with the functions that changed is linked ahead of it.
//!
//! The base gives its instances and statics names the patch can refer to. What the patch
//! defines under a name of the base replaces it there, for the linker takes the first
//! definition it sees. Whatever the patch can't express starts a full build over.

use std::hash::{Hash, Hasher};

use cranelift_module::{Linkage, Module};
use cranelift_object::{ObjectBuilder, ObjectModule};
use hir_def::{FunctionId, StaticId};
use rustc_hash::{FxHashMap, FxHashSet};

use crate::{db::HirDatabase, traits::StoredParamEnvAndCrate};

use super::{
    CodegenError, Instance, ObjectCodegen, OwnStatics, Phases, Rejections, Result, Roots,
    SymbolResolver, compiled_instance, compiled_root, compiled_static,
    fragment::{DataRequest, Requests},
    source_location, target,
};

/// What a compiled function or static was when the base took it in.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Mark {
    serial: u64,
    /// The source locations it reports, which stitching fills in.
    locations: u64,
}

impl Mark {
    fn new(db: &dyn HirDatabase, serial: u64, requests: &Requests) -> Mark {
        let mut hasher = rustc_hash::FxHasher::default();
        for request in &requests.datas {
            if let DataRequest::Location(Some((owner, span))) = request {
                source_location(db, *owner, *span).hash(&mut hasher);
            }
        }
        Mark { serial, locations: hasher.finish() }
    }
}

pub(super) struct BaseInstance {
    env: StoredParamEnvAndCrate,
    pub(super) symbol: String,
    /// `None` when the base calls rustc's copy and compiling the instance failed.
    mark: Option<Mark>,
    forwarded: bool,
}

pub(super) struct BaseStatic {
    pub(super) symbol: String,
    pub(super) writable: bool,
    mark: Mark,
}

/// What a function left to rustc keeps the rest of the build from doing.
#[derive(PartialEq, Eq)]
struct Left {
    /// Its code, when the function compiles and something else keeps it from the object.
    /// A full build looks at it again once that changes.
    code: Option<Mark>,
    statics: (FxHashSet<StaticId>, bool),
    async_blocks: Option<FxHashSet<salsa::Id>>,
}

impl Left {
    fn of(db: &dyn HirDatabase, root: FunctionId) -> Left {
        let code = compiled_root(db, root).as_ref().ok();
        Left {
            code: code.map(|it| Mark::new(db, it.serial, &it.requests)),
            statics: ObjectCodegen::statics_reachable_from(db, root),
            async_blocks: ObjectCodegen::async_blocks(db, root),
        }
    }
}

/// The last object built in full, as far as patching it goes.
pub(super) struct Base {
    entries: Vec<FunctionId>,
    pub(super) compiled: Vec<(FunctionId, String)>,
    marks: Vec<Mark>,
    pub(super) skipped: Vec<(FunctionId, CodegenError)>,
    /// For each of `skipped` that rustc has a copy of.
    left: Vec<Option<Left>>,
    pub(super) instances: FxHashMap<Instance, BaseInstance>,
    pub(super) statics: FxHashMap<StaticId, BaseStatic>,
    pub(super) bodies: usize,
}

pub(super) struct Patch {
    pub(super) object: Vec<u8>,
    pub(super) bodies: usize,
    pub(super) phases: Phases,
}

/// An object that defines nothing.
pub(super) fn empty_object() -> Result<Vec<u8>> {
    let builder =
        ObjectBuilder::new(target()?.clone(), "rustd", cranelift_module::default_libcall_names())?;
    ObjectModule::new(builder).finish().emit().map_err(|e| CodegenError::Backend(e.to_string()))
}

impl Base {
    /// `None` when the build has something in it a patch could not keep track of.
    pub(super) fn record(
        db: &dyn HirDatabase,
        cg: &ObjectCodegen<'_, '_>,
        entries: &[FunctionId],
        compiled: &[(FunctionId, String)],
        skipped: &[(FunctionId, CodegenError)],
    ) -> Option<Base> {
        let decls = cg.module.declarations();
        let marks = compiled
            .iter()
            .map(|&(root, _)| {
                let compiled = compiled_root(db, root).as_ref().ok()?;
                Some(Mark::new(db, compiled.serial, &compiled.requests))
            })
            .collect::<Option<Vec<_>>>()?;
        let instances = cg
            .envs
            .iter()
            .map(|(instance, env)| {
                let id = *cg.instances.get(instance)?;
                let compiled = compiled_instance(db, instance.clone(), env.clone()).as_ref().ok();
                let known = BaseInstance {
                    env: env.clone(),
                    symbol: decls.get_function_decl(id).linkage_name(id).into_owned(),
                    mark: compiled.map(|it| Mark::new(db, it.serial, &it.requests)),
                    forwarded: cg.forwarded.contains(instance),
                };
                (known.mark.is_some() || known.forwarded).then(|| (instance.clone(), known))
            })
            .collect::<Option<FxHashMap<_, _>>>()?;
        let statics = cg
            .statics
            .iter()
            .chain(&cg.frozen_statics)
            .map(|(&id, &data)| {
                let compiled = compiled_static(db, id).as_ref().ok()?;
                let known = BaseStatic {
                    symbol: decls.get_data_decl(data).linkage_name(data).into_owned(),
                    writable: compiled.writable,
                    mark: Mark::new(db, compiled.serial, &compiled.requests),
                };
                Some((id, known))
            })
            .collect::<Option<FxHashMap<_, _>>>()?;
        let left = skipped
            .iter()
            .map(|(root, e)| (!matches!(e, CodegenError::NoSymbol)).then(|| Left::of(db, *root)))
            .collect();
        Some(Base {
            entries: entries.to_vec(),
            compiled: compiled.to_vec(),
            marks,
            skipped: skipped.to_vec(),
            left,
            instances,
            statics,
            bodies: cg.bodies,
        })
    }

    pub(super) fn revalidate(&self, db: &dyn HirDatabase, part: usize, parts: usize) {
        let left = self.skipped.iter().zip(&self.left).filter(|it| it.1.is_some());
        let roots = self.compiled.iter().map(|it| it.0).chain(left.map(|it| it.0.0));
        for root in roots.skip(part).step_by(parts) {
            _ = compiled_root(db, root);
        }
        for (instance, was) in self.instances.iter().skip(part).step_by(parts) {
            _ = compiled_instance(db, instance.clone(), was.env.clone());
        }
    }

    /// The futures that functions left to rustc handle, or `None` when that can be any.
    fn their_async_blocks(&self) -> Option<Option<FxHashSet<salsa::Id>>> {
        let mut theirs = Some(FxHashSet::default());
        let mut any = false;
        for left in self.left.iter().flatten() {
            any = true;
            theirs = theirs.zip(left.async_blocks.as_ref()).map(|(mut all, blocks)| {
                all.extend(blocks);
                all
            });
        }
        any.then_some(theirs)
    }

    /// An object with what changed since the base was built, or why the change is more than
    /// a patch can hold.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn patch(
        &self,
        db: &dyn HirDatabase,
        symbols: &dyn SymbolResolver,
        roots: Roots<'_>,
        on_demand: &FxHashSet<FunctionId>,
        denied: &FxHashSet<FunctionId>,
        known: &Rejections,
        own_statics: OwnStatics,
    ) -> Result<std::result::Result<Patch, &'static str>> {
        let start = std::time::Instant::now();
        if roots.entries != self.entries {
            return Ok(Err("the roots compiled whether called or not changed"));
        }
        let all: FxHashSet<FunctionId> =
            roots.entries.iter().chain(roots.on_demand).copied().collect();
        let known_roots =
            || self.compiled.iter().map(|it| it.0).chain(self.skipped.iter().map(|it| it.0));
        if !known_roots().all(|root| all.contains(&root)) {
            return Ok(Err("a root of the base is gone"));
        }

        // What the base holds that is no longer what it was.
        let mut changed_roots = Vec::new();
        for (&(root, _), mark) in self.compiled.iter().zip(&self.marks) {
            let Ok(compiled) = compiled_root(db, root).as_ref() else {
                return Ok(Err("a root of the base no longer compiles"));
            };
            if Mark::new(db, compiled.serial, &compiled.requests) != *mark {
                changed_roots.push(root);
            }
        }
        let mut changed_instances = Vec::new();
        for (instance, was) in &self.instances {
            let compiled = compiled_instance(db, instance.clone(), was.env.clone()).as_ref().ok();
            let mark = compiled.map(|it| Mark::new(db, it.serial, &it.requests));
            if mark == was.mark {
                continue;
            }
            // One that had no code of its own might have now.
            if was.forwarded || mark.is_none() {
                return Ok(Err("an instance of the base gained or lost code of its own"));
            }
            changed_instances.push((instance, was, compiled));
        }
        for (&id, was) in &self.statics {
            let Ok(compiled) = compiled_static(db, id).as_ref() else {
                return Ok(Err("a static of the base no longer has a value"));
            };
            if Mark::new(db, compiled.serial, &compiled.requests) != was.mark {
                return Ok(Err("a static of the base changed"));
            }
        }
        for ((root, _), was) in self.skipped.iter().zip(&self.left) {
            if was.as_ref().is_some_and(|was| *was != Left::of(db, *root)) {
                return Ok(Err("a function left to rustc changed"));
            }
        }
        let verify = start.elapsed();

        let mut cg =
            ObjectCodegen::new(db, symbols, &known.rejected, own_statics, on_demand, Some(self))?;
        cg.phases.compile = verify;
        // rustc's copies are the ones the base was built against: what they call is in it.
        for (instance, was, compiled) in changed_instances {
            let Some(compiled) = compiled else {
                return Ok(Err("an instance of the base no longer compiles"));
            };
            let id = cg.module.declare_function(
                &was.symbol,
                Linkage::Hidden,
                &compiled.code.signature,
            )?;
            cg.instances.insert(instance.clone(), id);
            cg.pending.push((instance.clone(), was.env.clone(), id));
        }
        let mut queued: FxHashSet<FunctionId> = known_roots().collect();
        let mut queue = changed_roots;
        queue.extend(cg.wanted.drain(..).filter(|&func| queued.insert(func)));
        let mut next = 0;
        loop {
            while let Some(&root) = queue.get(next) {
                next += 1;
                if denied.contains(&root)
                    || known.left.contains(&root)
                    || cg.define_function(root).is_err()
                {
                    return Ok(Err("a root can't be compiled"));
                }
                queue.extend(cg.wanted.drain(..).filter(|&func| queued.insert(func)));
            }
            if !cg.define_instances().is_empty() {
                return Ok(Err("an instance can't be compiled"));
            }
            queue.extend(cg.wanted.drain(..).filter(|&func| queued.insert(func)));
            if next == queue.len() {
                break;
            }
        }
        // A future is laid out by one compiler: a function that now handles one of rustc's
        // has to be left to rustc as well.
        if let Some(theirs) = self.their_async_blocks() {
            let shares = |root| match (ObjectCodegen::async_blocks(db, root), &theirs) {
                (Some(ours), Some(theirs)) => !ours.is_disjoint(theirs),
                (Some(ours), None) => !ours.is_empty(),
                (None, _) => true,
            };
            if queue.iter().copied().any(shares) {
                return Ok(Err("a function now shares an `async` block with one left to rustc"));
            }
        }
        let (bodies, mut phases) = (cg.bodies, cg.phases);
        let start = std::time::Instant::now();
        let object = cg.finish()?;
        phases.object = start.elapsed();
        Ok(Ok(Patch { object, bodies, phases }))
    }
}
