//! Which code producer turns the IR of a function into machine code, and what each one did.
//!
//! A producer takes the Cranelift IR the front half builds from MIR, after layout and the
//! calling convention have been decided, and gives back a [`Code`]: bytes, the relocations
//! in them, and how to unwind through them. Nothing after that can tell the producers apart.
//! LLVM hands a function it has no lowering for to Cranelift, and says why.

use std::{
    collections::BTreeMap,
    fmt,
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use super::{CodegenError, Result, fragment::Code, fragment::Recorder};

/// Names the producer: `cranelift` (the default) or `llvm`.
const BACKEND: &str = "RUSTD_BACKEND";
/// How hard LLVM optimizes each function, `0` to `3`. Functions are compiled alone at any
/// level: nothing is inlined across them.
const OPTIMIZATION: &str = "RUSTD_OPT";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Cranelift,
    Llvm(Optimization),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Optimization {
    None,
    Less,
    Default,
    Aggressive,
}

impl Backend {
    /// The producer the environment asks for, or what is wrong with what it asks.
    pub fn from_env() -> std::result::Result<Backend, String> {
        static SELECTED: OnceLock<std::result::Result<Backend, String>> = OnceLock::new();
        SELECTED.get_or_init(Self::read_env).clone()
    }

    fn read_env() -> std::result::Result<Backend, String> {
        let optimization = match std::env::var(OPTIMIZATION).as_deref() {
            Ok("0") => Optimization::None,
            Ok("1") => Optimization::Less,
            Ok("2") | Err(_) => Optimization::Default,
            Ok("3") => Optimization::Aggressive,
            Ok(other) => return Err(format!("{OPTIMIZATION}={other}: expected 0, 1, 2 or 3")),
        };
        match std::env::var(BACKEND).as_deref() {
            Ok("cranelift") | Err(_) => Ok(Backend::Cranelift),
            Ok("llvm") if cfg!(feature = "llvm") => Ok(Backend::Llvm(optimization)),
            Ok("llvm") => Err(format!("{BACKEND}=llvm: rustd was built without `--features llvm`")),
            Ok(other) => Err(format!("{BACKEND}={other}: expected `cranelift` or `llvm`")),
        }
    }

    /// Compiles the function `context` holds. `recorder` is where a producer asks for a
    /// function its own code calls that the IR did not name.
    pub(super) fn emit(
        recorder: &mut Recorder<'_>,
        context: &mut cranelift_codegen::Context,
    ) -> Result<Code> {
        let tally = Tally::global();
        match Self::from_env().map_err(CodegenError::Backend)? {
            Backend::Cranelift => {}
            #[cfg(feature = "llvm")]
            Backend::Llvm(optimization) => {
                let start = Instant::now();
                match Self::emit_with_llvm(recorder, &context.func, optimization) {
                    Ok(code) => {
                        tally.llvm.add(start.elapsed(), code.bytes.len());
                        return Ok(code);
                    }
                    Err(unhandled) => tally.fell_back(start.elapsed(), &unhandled.to_string()),
                }
            }
            #[cfg(not(feature = "llvm"))]
            Backend::Llvm(_) => {}
        }
        _ = recorder;
        let start = Instant::now();
        let code = super::emit(context)?;
        tally.cranelift.add(start.elapsed(), code.bytes.len());
        Ok(code)
    }
}

#[cfg(feature = "llvm")]
impl Backend {
    /// What LLVM makes of the function, from the store if the same IR was compiled at the
    /// same level before.
    fn emit_with_llvm(
        recorder: &mut Recorder<'_>,
        func: &cranelift_codegen::ir::Function,
        optimization: Optimization,
    ) -> std::result::Result<Code, super::llvm::Unhandled> {
        use super::{cached::StoredCode, store::Store};
        let Some((store, key)) = Store::global().zip(StoredCode::key(optimization, func)) else {
            return super::llvm::emit(recorder, func, optimization);
        };
        if let Some(stored) = store.lookup(&key).and_then(StoredCode::decode)
            && let Some(code) = stored.restore(recorder)
        {
            return Ok(code);
        }
        let mark = recorder.mark();
        let code = super::llvm::emit(recorder, func, optimization)?;
        if let Some(stored) = StoredCode::of(&code, recorder, mark).and_then(|it| it.encode()) {
            store.insert(&key, &stored);
        }
        Ok(code)
    }
}

impl fmt::Display for Backend {
    /// Also what tells the entries of one producer from another's in the store.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Backend::Cranelift => f.write_str("cranelift"),
            Backend::Llvm(Optimization::None) => f.write_str("llvm -O0"),
            Backend::Llvm(Optimization::Less) => f.write_str("llvm -O1"),
            Backend::Llvm(Optimization::Default) => f.write_str("llvm -O2"),
            Backend::Llvm(Optimization::Aggressive) => f.write_str("llvm -O3"),
        }
    }
}

/// What the producers did since the last report.
#[derive(Default)]
pub(super) struct Tally {
    cranelift: Produced,
    llvm: Produced,
    /// Time LLVM spent on functions it then gave up on.
    wasted: AtomicU64,
    /// Why LLVM left a function to Cranelift, and how often.
    fallbacks: Mutex<BTreeMap<String, usize>>,
    /// Where LLVM's time went.
    pub(super) phases: [AtomicU64; Phase::COUNT],
}

#[derive(Default)]
struct Produced {
    functions: AtomicUsize,
    nanos: AtomicU64,
    bytes: AtomicUsize,
}

/// The steps of compiling one function with LLVM.
#[derive(Debug, Clone, Copy)]
pub(super) enum Phase {
    Lower,
    Verify,
    Optimize,
    Emit,
    Read,
}

impl Phase {
    const COUNT: usize = 5;
    const ALL: [Phase; Phase::COUNT] =
        [Phase::Lower, Phase::Verify, Phase::Optimize, Phase::Emit, Phase::Read];
}

impl Produced {
    fn add(&self, took: Duration, bytes: usize) {
        self.functions.fetch_add(1, Ordering::Relaxed);
        self.nanos.fetch_add(took.as_nanos() as u64, Ordering::Relaxed);
        self.bytes.fetch_add(bytes, Ordering::Relaxed);
    }

    fn take(&self) -> Option<String> {
        let functions = self.functions.swap(0, Ordering::Relaxed);
        let took = Duration::from_nanos(self.nanos.swap(0, Ordering::Relaxed));
        let bytes = self.bytes.swap(0, Ordering::Relaxed);
        (functions != 0).then(|| {
            let each = took / functions as u32;
            format!("{functions} functions, {bytes} bytes, {took:.1?} ({each:.1?} each)")
        })
    }
}

impl Tally {
    pub(super) fn global() -> &'static Tally {
        static TALLY: OnceLock<Tally> = OnceLock::new();
        TALLY.get_or_init(Tally::default)
    }

    #[cfg(feature = "llvm")]
    fn fell_back(&self, took: Duration, reason: &str) {
        self.wasted.fetch_add(took.as_nanos() as u64, Ordering::Relaxed);
        let mut fallbacks = self.fallbacks.lock().unwrap_or_else(|e| e.into_inner());
        *fallbacks.entry(reason.to_owned()).or_default() += 1;
    }

    #[cfg(feature = "llvm")]
    pub(super) fn time<T>(&self, phase: Phase, work: impl FnOnce() -> T) -> T {
        let start = Instant::now();
        let out = work();
        self.phases[phase as usize].fetch_add(start.elapsed().as_nanos() as u64, Ordering::Relaxed);
        out
    }

    /// Says what was compiled by which producer since the last call, on stderr. Times are
    /// summed over the threads that compiled.
    pub(super) fn report(&self) {
        let fallbacks =
            std::mem::take(&mut *self.fallbacks.lock().unwrap_or_else(|e| e.into_inner()));
        let wasted = Duration::from_nanos(self.wasted.swap(0, Ordering::Relaxed));
        let phases = Phase::ALL.map(|phase| {
            Duration::from_nanos(self.phases[phase as usize].swap(0, Ordering::Relaxed))
        });
        let (llvm, cranelift) = (self.llvm.take(), self.cranelift.take());
        let selected = Self::selected_llvm();
        if !selected && std::env::var_os("RUSTD_STATS").is_none() {
            return;
        }
        if let Some(llvm) = llvm {
            let phases = std::iter::zip(Phase::ALL, phases)
                .map(|(phase, took)| format!("{} {took:.1?}", format!("{phase:?}").to_lowercase()))
                .collect::<Vec<_>>()
                .join(", ");
            eprintln!("backend: llvm compiled {llvm}: {phases}");
        }
        if let Some(cranelift) = cranelift {
            eprintln!("backend: cranelift compiled {cranelift}");
        }
        if !fallbacks.is_empty() {
            let count: usize = fallbacks.values().sum();
            eprintln!("backend: llvm left {count} functions to cranelift after {wasted:.1?}:");
            for (reason, count) in fallbacks {
                eprintln!("  {count}\t{reason}");
            }
        }
    }

    fn selected_llvm() -> bool {
        matches!(Backend::from_env(), Ok(Backend::Llvm(_)))
    }
}
