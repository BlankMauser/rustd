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
    SymbolResolver, TheirStatics, async_blocks, compiled_instance, compiled_root, compiled_static,
    fragment::{DataRequest, FuncTarget, Requests},
    left_to_rustc, source_location, target,
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

/// Something in the base that an edit can change or take away.
#[derive(Clone, PartialEq, Eq, Hash)]
enum Node {
    Root(FunctionId),
    Instance(Instance),
    Static(StaticId),
}

impl Node {
    fn is_asked_for_by(&self, requests: &Requests) -> bool {
        match self {
            Node::Root(_) => false,
            Node::Instance(instance) => requests.funcs.iter().any(
                |it| matches!(&it.target, FuncTarget::Instance(asked, _) if asked == instance),
            ),
            Node::Static(id) => requests
                .datas
                .iter()
                .any(|it| matches!(it, DataRequest::Static(asked) if asked == id)),
        }
    }
}

/// What a query gives for something of the base, or `None` when it panics: the edit may have
/// taken away what the base knew it by, and only looking tells.
fn probe<T>(query: impl FnOnce() -> T) -> Option<T> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(query)).ok()
}

pub(super) struct BaseInstance {
    env: StoredParamEnvAndCrate,
    pub(super) symbol: String,
    /// `None` when the base calls rustc's copy and compiling the instance failed.
    mark: Option<Mark>,
    forwarded: bool,
    /// What in the base refers to it.
    parents: Vec<Node>,
}

pub(super) struct BaseStatic {
    pub(super) symbol: String,
    pub(super) writable: bool,
    mark: Mark,
    parents: Vec<Node>,
}

/// What a function left to rustc keeps the rest of the build from doing.
#[derive(PartialEq, Eq)]
struct Left {
    /// Its code, when the function compiles and something else keeps it from the object.
    /// A full build looks at it again once that changes.
    code: Option<Mark>,
    async_blocks: Option<FxHashSet<salsa::Id>>,
}

impl Left {
    fn of(db: &dyn HirDatabase, root: FunctionId) -> Left {
        let code = compiled_root(db, root).as_ref().ok();
        Left {
            code: code.map(|it| Mark::new(db, it.serial, &it.requests)),
            async_blocks: async_blocks(db, root).clone(),
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
    their_statics: TheirStatics,
    pub(super) instances: FxHashMap<Instance, BaseInstance>,
    pub(super) statics: FxHashMap<StaticId, BaseStatic>,
    pub(super) bodies: usize,
}

/// What a look at a part of the base found changed.
#[derive(Default)]
pub struct Stale {
    roots: Vec<FunctionId>,
    instances: Vec<Instance>,
    /// What a patch can't replace, and why. That only matters for what the program still
    /// reaches.
    broken: Vec<(Node, &'static str)>,
    /// Why no patch can hold the change.
    declined: Option<&'static str>,
    pub steps: StaleSteps,
}

/// How long looking at each kind of thing in the base took, in the order they are looked at.
#[derive(Default, Clone, Copy)]
pub struct StaleSteps {
    pub roots: std::time::Duration,
    pub instances: std::time::Duration,
    pub statics: std::time::Duration,
    /// The functions left to rustc.
    pub left: std::time::Duration,
}

impl Stale {
    /// Declines when the functions the base left to rustc reach other statics than they did.
    fn their_statics(&mut self, base: &Base, db: &dyn HirDatabase) {
        let now = probe(|| TheirStatics::of(db, left_to_rustc(&base.skipped)));
        if now.as_ref() != Some(&base.their_statics) {
            self.declined = Some("the statics of a function left to rustc changed");
        }
    }

    fn declined(mut self, why: &'static str) -> Stale {
        self.declined = Some(why);
        self
    }
}

/// What of the base an edit left as it was: everything but these.
struct Unchanged {
    changed: FxHashSet<Node>,
    broken: FxHashSet<Node>,
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
        let mut instances = cg
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
                    parents: Vec::new(),
                };
                (known.mark.is_some() || known.forwarded).then(|| (instance.clone(), known))
            })
            .collect::<Option<FxHashMap<_, _>>>()?;
        let mut statics = cg
            .statics
            .iter()
            .chain(&cg.frozen_statics)
            .map(|(&id, &data)| {
                let compiled = compiled_static(db, id).as_ref().ok()?;
                let known = BaseStatic {
                    symbol: decls.get_data_decl(data).linkage_name(data).into_owned(),
                    writable: compiled.writable,
                    mark: Mark::new(db, compiled.serial, &compiled.requests),
                    parents: Vec::new(),
                };
                Some((id, known))
            })
            .collect::<Option<FxHashMap<_, _>>>()?;
        let mut edges = Vec::new();
        let mut note = |parent: Node, requests: &Requests| {
            for request in &requests.funcs {
                if let FuncTarget::Instance(instance, _) = &request.target {
                    edges.push((Node::Instance(instance.clone()), parent.clone()));
                }
            }
            for request in &requests.datas {
                if let DataRequest::Static(id) = request {
                    edges.push((Node::Static(*id), parent.clone()));
                }
            }
        };
        for &(root, _) in compiled {
            note(Node::Root(root), &compiled_root(db, root).as_ref().ok()?.requests);
        }
        for (instance, known) in &instances {
            if let Ok(compiled) = compiled_instance(db, instance.clone(), known.env.clone()) {
                note(Node::Instance(instance.clone()), &compiled.requests);
            }
        }
        for &id in statics.keys() {
            note(Node::Static(id), &compiled_static(db, id).as_ref().ok()?.requests);
        }
        for (child, parent) in edges {
            let parents = match &child {
                Node::Root(_) => None,
                Node::Instance(instance) => instances.get_mut(instance).map(|it| &mut it.parents),
                Node::Static(id) => statics.get_mut(id).map(|it| &mut it.parents),
            };
            if let Some(parents) = parents {
                parents.push(parent);
            }
        }
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
            their_statics: TheirStatics::of(db, left_to_rustc(skipped)),
            instances,
            statics,
            bodies: cg.bodies,
        })
    }

    /// What one part in `parts` of the base no longer is what it was. Looking is most of the
    /// work of a patch, and the parts can be looked at on a thread each.
    pub(super) fn stale(&self, db: &dyn HirDatabase, part: usize, parts: usize) -> Stale {
        let start = std::time::Instant::now();
        let mut stale = Stale::default();
        let every = |len: usize| (part..len).step_by(parts);
        for i in every(self.compiled.len()) {
            let root = self.compiled[i].0;
            let mark = probe(|| {
                let compiled = compiled_root(db, root).as_ref().ok()?;
                Some(Mark::new(db, compiled.serial, &compiled.requests))
            });
            match mark {
                Some(Some(mark)) if mark == self.marks[i] => {}
                Some(Some(_)) => stale.roots.push(root),
                Some(None) => return stale.declined("a root of the base no longer compiles"),
                None => return stale.declined("a root of the base is gone"),
            }
        }
        stale.steps.roots = start.elapsed();
        for (instance, was) in self.instances.iter().skip(part).step_by(parts) {
            let mark = probe(|| {
                let compiled = compiled_instance(db, instance.clone(), was.env.clone());
                compiled.as_ref().ok().map(|it| Mark::new(db, it.serial, &it.requests))
            });
            match mark {
                Some(mark) if mark == was.mark => {}
                Some(Some(_)) if !was.forwarded => stale.instances.push(instance.clone()),
                // One that had no code of its own might have now.
                _ => stale.broken.push((
                    Node::Instance(instance.clone()),
                    "an instance of the base gained or lost code of its own",
                )),
            }
        }
        stale.steps.instances = start.elapsed() - stale.steps.roots;
        for (&id, was) in self.statics.iter().skip(part).step_by(parts) {
            let mark = probe(|| {
                let compiled = compiled_static(db, id).as_ref().ok()?;
                Some(Mark::new(db, compiled.serial, &compiled.requests))
            });
            if mark != Some(Some(was.mark)) {
                stale.broken.push((Node::Static(id), "a static of the base changed"));
            }
        }
        stale.steps.statics = start.elapsed() - stale.steps.roots - stale.steps.instances;
        for i in every(self.skipped.len()) {
            let Some(was) = &self.left[i] else { continue };
            if probe(|| Left::of(db, self.skipped[i].0)).as_ref() != Some(was) {
                return stale.declined("a function left to rustc changed");
            }
        }
        if part == 0 {
            stale.their_statics(self, db);
        }
        stale.steps.left =
            start.elapsed() - stale.steps.roots - stale.steps.instances - stale.steps.statics;
        stale
    }

    /// Whether a root still refers to the node, directly or through others.
    fn reaches(&self, db: &dyn HirDatabase, node: &Node, unchanged: &Unchanged) -> bool {
        let mut seen = FxHashSet::default();
        let mut stack = vec![node];
        while let Some(node) = stack.pop() {
            let parents = match node {
                Node::Root(_) => return true,
                Node::Instance(instance) => self.instances.get(instance).map(|it| &it.parents),
                Node::Static(id) => self.statics.get(id).map(|it| &it.parents),
            };
            for parent in parents.into_iter().flatten() {
                if !seen.contains(parent) && self.still_asks_for(db, parent, node, unchanged) {
                    seen.insert(parent);
                    stack.push(parent);
                }
            }
        }
        false
    }

    /// Whether what referred to the node in the base does so after the edit.
    fn still_asks_for(
        &self,
        db: &dyn HirDatabase,
        parent: &Node,
        node: &Node,
        unchanged: &Unchanged,
    ) -> bool {
        // A broken parent that is reached stops the patch by itself.
        if unchanged.broken.contains(parent) {
            return false;
        }
        if !unchanged.changed.contains(parent) {
            return true;
        }
        let requests = match parent {
            Node::Root(root) => compiled_root(db, *root).as_ref().ok().map(|it| &it.requests),
            Node::Instance(instance) => self.instances.get(instance).and_then(|known| {
                let compiled = compiled_instance(db, instance.clone(), known.env.clone());
                compiled.as_ref().ok().map(|it| &it.requests)
            }),
            Node::Static(_) => None,
        };
        requests.is_some_and(|requests| node.is_asked_for_by(requests))
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
        stale: Vec<Stale>,
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

        let mut changed_roots = Vec::new();
        let mut changed_instances = Vec::new();
        let mut broken = Vec::new();
        for part in stale {
            if let Some(why) = part.declined {
                return Ok(Err(why));
            }
            changed_roots.extend(part.roots);
            changed_instances.extend(part.instances);
            broken.extend(part.broken);
        }
        // What the edit took away is still in the base, where nothing refers to it any more.
        let unchanged = Unchanged {
            changed: changed_roots
                .iter()
                .map(|&root| Node::Root(root))
                .chain(changed_instances.iter().cloned().map(Node::Instance))
                .collect(),
            broken: broken.iter().map(|it| it.0.clone()).collect(),
        };
        if let Some((_, why)) = broken.iter().find(|it| self.reaches(db, &it.0, &unchanged)) {
            return Ok(Err(why));
        }
        changed_instances.retain(|it| self.reaches(db, &Node::Instance(it.clone()), &unchanged));
        let verify = start.elapsed();

        let mut cg =
            ObjectCodegen::new(db, symbols, &known.rejected, own_statics, on_demand, Some(self))?;
        cg.phases.compile = verify;
        // rustc's copies are the ones the base was built against: what they call is in it.
        for instance in &changed_instances {
            let was = &self.instances[instance];
            let Ok(compiled) = compiled_instance(db, instance.clone(), was.env.clone()).as_ref()
            else {
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
            let shares = |root| match (async_blocks(db, root).clone(), &theirs) {
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
