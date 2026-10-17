//! Features of the language that became stable between Rust 1.85 and 1.99.
use std::fmt::Debug;

// let_chains (1.88)
/// A spin loop's hint to the processor, which is `pause` through an LLVM intrinsic on x86.
pub fn spin_hint() -> u32 {
    let mut spins = 0;
    while spins < 3 {
        std::hint::spin_loop();
        spins += 1;
    }
    spins
}

pub fn let_chains() -> Vec<u32> {
    let values = [Some(3u32), None, Some(10), Some(7)];
    let mut out = Vec::new();
    for (i, v) in values.iter().enumerate() {
        if let Some(x) = v && *x > 5 && let Some(y) = values.get(i.wrapping_sub(1)) && let Some(y) = y {
            out.push(x + y);
        } else if let Some(x) = v && x % 2 == 1 {
            out.push(*x);
        } else {
            out.push(0);
        }
    }
    let mut it = values.iter();
    while let Some(v) = it.next() && let Some(x) = v {
        out.push(*x * 100);
    }
    out
}

// if_let_guard (1.95)
pub fn if_let_guard() -> Vec<String> {
    let inputs = [("a", "12"), ("b", "x"), ("a", "-4"), ("c", "9")];
    inputs
        .iter()
        .map(|&(k, v)| match k {
            "a" if let Ok(n) = v.parse::<i32>() && n > 0 => format!("pos {n}"),
            "a" if let Ok(n) = v.parse::<i32>() => format!("neg {n}"),
            other if let Err(e) = v.parse::<i32>() => format!("{other} bad {}", e.to_string().len()),
            other => format!("{other} ok"),
        })
        .collect()
}

// trait_upcasting (1.86)
trait Base: Debug {
    fn base(&self) -> u32;
}
trait Mid: Base {
    fn mid(&self) -> u32;
}
trait Other {
    fn other(&self) -> u32;
}
trait Top: Mid + Other + Send {
    fn top(&self) -> u32;
}
#[derive(Debug)]
struct Thing(u32);
impl Base for Thing {
    fn base(&self) -> u32 {
        self.0
    }
}
impl Mid for Thing {
    fn mid(&self) -> u32 {
        self.0 * 2
    }
}
impl Other for Thing {
    fn other(&self) -> u32 {
        self.0 * 3
    }
}
impl Top for Thing {
    fn top(&self) -> u32 {
        self.0 * 4
    }
}
pub fn trait_upcasting() -> (u32, u32, u32, u32, String, u32) {
    let top: Box<dyn Top> = Box::new(Thing(5));
    let as_ref: &dyn Top = &*top;
    let mid: &dyn Mid = as_ref;
    let other: &dyn Other = as_ref;
    let base: &dyn Base = mid;
    let debug: &dyn Debug = base;
    let rc: std::rc::Rc<dyn Top> = std::rc::Rc::new(Thing(9));
    let rc_other: std::rc::Rc<dyn Other> = rc;
    let boxed: Box<dyn Base> = Box::new(Thing(6)) as Box<dyn Top>;
    (mid.mid(), other.other(), base.base(), boxed.base(), format!("{debug:?}"), rc_other.other())
}

// cfg_boolean_literals (1.88) and cfg_select (1.95)
#[cfg(true)]
fn cfg_true() -> u32 {
    1
}
#[cfg(false)]
fn cfg_true() -> u32 {
    2
}
pub fn cfg_select() -> (u32, &'static str, u32) {
    let name = cfg_select! {
        windows => "windows",
        unix => "unix",
        _ => "other",
    };
    cfg_select! {
        target_pointer_width = "16" => { let w = 16; }
        target_pointer_width = "64" => { let w = 64; }
        _ => { let w = 32; }
    }
    (cfg_true(), name, w)
}

// generic_arg_infer (1.89)
fn make<const N: usize>(v: u8) -> [u8; N] {
    [v; N]
}
pub fn generic_arg_infer() -> (usize, Vec<u16>) {
    let a: [u8; _] = make::<4>(1);
    let b: [u8; 3] = make::<_>(2);
    let c = [7u16; _];
    let d: [u16; 2] = c;
    let e: Vec<_> = Vec::<_>::from(d);
    (a.len() + b.len(), e)
}

// repr128 (1.89)
#[repr(u128)]
#[derive(Debug, Clone, Copy, PartialEq)]
enum Wide {
    Low = 1,
    High = 1 << 100,
    Top = u128::MAX,
}
#[repr(i128)]
#[derive(Debug, Clone, Copy)]
enum Signed {
    Neg(u8) = -(1 << 90),
    Pos { x: u16 } = 1 << 90,
}
pub fn repr128() -> (u128, u128, usize, usize, String, bool, usize) {
    let values = [Wide::Low, Wide::High, Wide::Top];
    let s = [Signed::Neg(3), Signed::Pos { x: 9 }];
    let pick = std::hint::black_box(values[1]);
    let named = match pick {
        Wide::Low => "low",
        Wide::High => "high",
        Wide::Top => "top",
    };
    (
        pick as u128,
        values[2] as u128,
        size_of::<Wide>(),
        size_of::<Option<Wide>>(),
        format!("{named} {:?}", std::hint::black_box(s)),
        std::hint::black_box(values[0]) == Wide::Low,
        size_of::<Signed>(),
    )
}

// precise_capturing_in_traits (1.87)
trait Source {
    fn items<'a>(&'a self, skip: &u32) -> impl Iterator<Item = u32> + use<'a, Self>;
}
impl Source for Vec<u32> {
    fn items<'a>(&'a self, skip: &u32) -> impl Iterator<Item = u32> + use<'a> {
        let skip = *skip;
        self.iter().copied().filter(move |x| *x != skip)
    }
}
fn capture_less<'a, T: Debug>(x: &'a T, n: u32) -> impl Iterator<Item = u32> + use<T> {
    let len = format!("{x:?}").len() as u32;
    (0..n).map(move |i| i + len)
}
pub fn precise_capturing() -> (Vec<u32>, Vec<u32>) {
    let v = vec![1, 2, 3, 2];
    let it = {
        let skip = 2;
        Source::items(&v, &skip)
    };
    let other = {
        let s = String::from("ab");
        capture_less(&s, 3)
    };
    (it.collect(), other.collect())
}

// c_variadic (1.99)
#[cfg(not(target_family = "wasm"))]
pub unsafe extern "C" fn sum_all(count: u32, mut args: ...) -> f64 {
    let mut total = 0.0;
    for i in 0..count {
        if i % 2 == 0 {
            total += unsafe { args.next_arg::<i32>() } as f64;
        } else {
            total += unsafe { args.next_arg::<f64>() };
        }
    }
    total
}
#[cfg(not(target_family = "wasm"))]
unsafe extern "C" fn forward(count: u32, mut args: ...) -> u64 {
    let mut copy = args.clone();
    let first = unsafe { args.next_arg::<u64>() };
    let mut rest = 0;
    for _ in 0..count {
        rest += unsafe { copy.next_arg::<u64>() };
    }
    first * 1000 + rest
}
#[cfg(not(target_family = "wasm"))]
pub fn c_variadic() -> (f64, u64, f64) {
    unsafe {
        (
            sum_all(4, 1i32, 2.5f64, 3i32, 4.25f64),
            forward(3, 7u64, 8u64, 9u64),
            sum_all(12, 1i32, 1.0f64, 2i32, 2.0f64, 3i32, 3.0f64, 4i32, 4.0f64, 5i32, 5.0f64, 6i32, 6.0f64),
        )
    }
}

// never type as far as stable names it
fn diverge(x: u32) -> ! {
    panic!("never {x}")
}
type Never = <fn() -> ! as FnOnceOut>::Out;
trait FnOnceOut {
    type Out;
}
impl<R> FnOnceOut for fn() -> R {
    type Out = R;
}
fn absurd(n: Never) -> u32 {
    n
}
fn infallible(r: Result<u32, Never>) -> u32 {
    let Ok(v) = r;
    v
}
fn from_inf(r: Result<u8, std::convert::Infallible>) -> u8 {
    match r {
        Ok(v) => v,
    }
}
pub fn never() -> (u32, u8, usize, usize, String, u32, u32) {
    let f: fn(u32) -> ! = diverge;
    let caught = std::panic::catch_unwind(|| {
        let x: u32 = if std::hint::black_box(true) { f(3) } else { 4 };
        x
    });
    let msg = match caught {
        Err(e) => e.downcast_ref::<String>().cloned().unwrap_or_default(),
        Ok(v) => v.to_string(),
    };
    let none: Option<Never> = None;
    let v: Vec<Never> = Vec::new();
    let looped = loop {
        break 5;
    };
    let early = (|| -> u32 {
        let x: u32 = match std::hint::black_box(Some(2)) {
            Some(n) => n,
            None => return 0,
        };
        let _unreached: fn(Never) -> u32 = absurd;
        x + v.len() as u32 + none.map_or(0, absurd)
    })();
    (
        infallible(Ok(9)),
        from_inf(Ok(4)),
        size_of::<Never>() + size_of::<Result<u32, Never>>(),
        size_of::<Option<Never>>() + size_of::<(u64, Never)>() + align_of::<Never>(),
        msg,
        looped,
        early,
    )
}

pub fn pc_trait() -> Vec<u32> {
    let v = vec![1, 2, 3, 2];
    let it = { let skip = 2; Source::items(&v, &skip) };
    it.collect()
}
pub fn pc_free() -> Vec<u32> {
    let other = { let s = String::from("ab"); capture_less(&s, 3) };
    other.collect()
}
pub fn pc_plain() -> Vec<u32> {
    let v = vec![1, 2, 3, 2];
    Source::items(&v, &2).collect()
}

trait S2 { fn items<'a>(&'a self, skip: &u32) -> impl Iterator<Item = u32>; }
impl S2 for Vec<u32> {
    fn items<'a>(&'a self, skip: &u32) -> impl Iterator<Item = u32> {
        let skip = *skip;
        self.iter().copied().filter(move |x| *x != skip)
    }
}
trait S3 { fn items<'a>(&'a self, skip: &u32) -> impl Iterator<Item = u32> + use<'a, Self>; }
impl S3 for Vec<u32> {
    fn items<'a>(&'a self, skip: &u32) -> impl Iterator<Item = u32> + use<'a, > {
        let skip = *skip;
        (0..3u32).filter(move |x| *x != skip)
    }
}
trait S4 { fn items(&self, skip: u32) -> impl Iterator<Item = u32>; }
impl S4 for Vec<u32> {
    fn items(&self, skip: u32) -> impl Iterator<Item = u32> {
        self.iter().copied().filter(move |x| *x != skip)
    }
}
pub fn pc_s2() -> Vec<u32> { let v = vec![1, 2, 3, 2]; S2::items(&v, &2).collect() }
pub fn pc_s3() -> Vec<u32> { let v = vec![1, 2, 3, 2]; S3::items(&v, &2).collect() }
pub fn pc_s4() -> Vec<u32> { let v = vec![1, 2, 3, 2]; S4::items(&v, 2).collect() }

trait Named { fn name() -> &'static str; }
impl Named for () { fn name() -> &'static str { "unit" } }
impl Named for Never { fn name() -> &'static str { "never" } }
impl Named for u32 { fn name() -> &'static str { "u32" } }
fn produced<X: Named>(_: impl FnOnce() -> X) -> &'static str { X::name() }
#[derive(Debug)]
enum Sparse { A(u8), B(Never), C(u16, Never), D }
fn try_never(r: Result<u8, Never>) -> Result<u8, String2> {
    Ok(r? + 1)
}
impl From<Never> for String2 { fn from(n: Never) -> Self { match n {} } }
struct String2;
fn or_die(v: Option<u32>) -> u32 {
    v.unwrap_or_else(|| diverge(77))
}
pub fn never_more() -> (Vec<&'static str>, usize, String, u8, u32, bool, String) {
    let names = vec![
        produced(|| panic!()),
        produced(|| loop { panic!() }),
        produced(|| -> u32 { panic!() }),
        produced(|| if std::hint::black_box(true) { 1u32 } else { panic!() }),
        produced(|| diverge(1)),
    ];
    let sparse = [Sparse::A(1), Sparse::D];
    let r: Result<u8, Never> = Ok(4);
    let Ok(direct) = r;
    let ptr: fn(u32) -> ! = diverge;
    let as_any: fn(u32) -> u32 = |x| if x > 0 { x } else { diverge(x) };
    let empty: [Never; 0] = [];
    let refd: Option<&Never> = empty.first();
    let caught = std::panic::catch_unwind(|| or_die(None)).unwrap_err();
    (
        names,
        size_of::<Sparse>() + size_of::<Option<Sparse>>() + size_of::<[Never; 4]>() + size_of::<&Never>(),
        format!("{sparse:?}"),
        try_never(Ok(2)).unwrap_or(0) + direct,
        as_any(6) + or_die(Some(1)),
        refd.is_none() && ptr as usize != 0,
        caught.downcast_ref::<String>().cloned().unwrap_or_default(),
    )
}

#[cfg(unix)]
pub fn variadic_floats() -> Vec<String> {
    use std::ffi::{c_char, c_int, CStr};
    unsafe extern "C" { fn snprintf(buf: *mut c_char, n: usize, fmt: *const c_char, ...) -> c_int; }
    let mut out = Vec::new();
    let mut buf = [0 as c_char; 128];
    unsafe {
        snprintf(buf.as_mut_ptr(), 128, c"%.2f %d %.1f %s".as_ptr(), 1.5f64, 7i32, -2.25f64, c"x".as_ptr());
        out.push(CStr::from_ptr(buf.as_ptr()).to_string_lossy().into_owned());
        snprintf(buf.as_mut_ptr(), 128, c"%g %g %g %g %g %g %g %g".as_ptr(), 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0);
        out.push(CStr::from_ptr(buf.as_ptr()).to_string_lossy().into_owned());
        snprintf(buf.as_mut_ptr(), 128, c"%d %d %g".as_ptr(), 1i32, 2i32, 0.5f64);
        out.push(CStr::from_ptr(buf.as_ptr()).to_string_lossy().into_owned());
        let through: unsafe extern "C" fn(*mut c_char, usize, *const c_char, ...) -> c_int = snprintf;
        through(buf.as_mut_ptr(), 128, c"%g %d %g".as_ptr(), 2.5f64, 3i32, -0.125f64);
        out.push(CStr::from_ptr(buf.as_ptr()).to_string_lossy().into_owned());
    }
    out
}

// More than the registers hold, so the rest is read from the stack.
#[cfg(not(target_family = "wasm"))]
unsafe extern "C" fn weigh(a: f64, b: *const u8, count: u32, mut args: ...) -> f64 {
    let mut total = a + unsafe { *b } as f64;
    for at in 0..count {
        total = total * 3.0
            + match at % 3 {
                0 => unsafe { args.next_arg::<i64>() as f64 },
                1 => unsafe { args.next_arg::<f64>() },
                _ => unsafe { *args.next_arg::<*const u16>() as f64 },
            };
    }
    total
}
#[cfg(not(target_family = "wasm"))]
pub fn c_variadic_stack() -> (f64, f64, f64) {
    let (b, p) = (9u8, 513u16);
    let through: unsafe extern "C" fn(f64, *const u8, u32, ...) -> f64 = weigh;
    unsafe {
        (
            weigh(0.5, &b, 0),
            weigh(
                0.5, &b, 30, 1i64, 0.25f64, &p, 2i64, 0.5f64, &p, 3i64, 0.75f64, &p, 4i64, 1.0f64,
                &p, 5i64, 1.25f64, &p, 6i64, 1.5f64, &p, 7i64, 1.75f64, &p, 8i64, 2.0f64, &p, 9i64,
                2.25f64, &p, 10i64, 2.5f64, &p,
            ),
            through(1.5, &b, 4, -1i64, 8.5f64, &p, i64::MAX),
        )
    }
}

#[cfg(all(target_arch = "x86_64", unix))]
pub mod x86 {
    use std::arch::{asm, naked_asm};

    // naked_functions (1.88)
    #[unsafe(naked)]
    pub extern "C" fn naked_add(a: u64, b: u64) -> u64 {
        naked_asm!("lea rax, [rdi + rsi]", "ret")
    }
    pub fn naked_functions() -> u64 {
        naked_add(40, 2)
    }

    // asm_goto (1.87)
    pub fn asm_goto() -> u32 {
        let mut out = 0;
        for x in [0u64, 5] {
            unsafe {
                asm!("test {0}, {0}", "jnz {1}", in(reg) x, label { out += 10 }, options(nostack));
            }
            out += 1;
        }
        out
    }

    // asm_cfg (1.93)
    pub fn asm_cfg() -> u64 {
        let mut v: u64 = 1;
        unsafe {
            asm!(
                "add {0}, 2",
                #[cfg(false)]
                "add {0}, 100",
                #[cfg(target_arch = "x86_64")]
                "add {0}, 4",
                inout(reg) v,
            );
        }
        v
    }

    // target_feature_11 (1.86)
    #[target_feature(enable = "avx2")]
    fn with_avx2(a: &[u32; 8]) -> u32 {
        a.iter().map(|x| x.wrapping_mul(3)).sum()
    }
    #[target_feature(enable = "avx2")]
    fn calls_safely(a: &[u32; 8]) -> u32 {
        let f: unsafe fn(&[u32; 8]) -> u32 = with_avx2;
        with_avx2(a) + (|| with_avx2(a))()
    }
    pub fn target_feature_11() -> u32 {
        let a = [1, 2, 3, 4, 5, 6, 7, 8];
        if is_x86_feature_detected!("avx2") { unsafe { calls_safely(&a) } } else { 216 }
    }

    // calling variadics with other ABIs (extended_varargs_abi_support 1.91)
    unsafe extern "sysv64" {
        #[link_name = "snprintf"]
        fn snprintf_sysv(buf: *mut u8, n: usize, fmt: *const u8, ...) -> i32;
    }
    pub fn varargs_abi() -> String {
        let mut buf = [0u8; 32];
        let n = unsafe { snprintf_sysv(buf.as_mut_ptr(), 32, c"%d-%s-%.1f".as_ptr().cast(), 7i32, c"x".as_ptr(), 1.5f64) };
        String::from_utf8_lossy(&buf[..n as usize]).into_owned()
    }

    #[unsafe(naked)]
    pub extern "C" fn naked_att(a: u64) -> u64 {
        naked_asm!("leaq {k}(%rdi), %rax", "ret", k = const NAKED_K, options(att_syntax))
    }
    #[unsafe(naked)]
    pub extern "C" fn naked_float(a: f64, b: f64) -> f64 {
        naked_asm!("addsd xmm0, xmm1", "ret")
    }
    const NAKED_K: u64 = 5 + 2;
    pub fn naked_more() -> (u64, f64, u64) {
        let f: extern "C" fn(u64) -> u64 = naked_att;
        (naked_att(10), naked_float(1.5, 2.25), f(1))
    }

    pub fn asm_goto_more() -> Vec<u32> {
        let mut out = Vec::new();
        for x in 0u64..4 {
            let mut hit = 0u32;
            unsafe {
                asm!(
                    "cmp {x}, 1", "je {one}", "cmp {x}, 2", "je {two}",
                    x = in(reg) x,
                    one = label { out.push(100) },
                    two = label { out.push(200); continue },
                );
            }
            out.push(hit);
        }
        for x in 0u64..3 {
            let v = 'found: {
                unsafe {
                    asm!("test {0}, {0}", "jnz {1}", "jmp {2}", in(reg) x,
                         label { break 'found x as u32 * 3 }, label { break 'found 77 },
                         options(noreturn));
                }
                99
            };
            out.push(v);
        }
        out
    }

    /// A symbol in the assembly of a function that is nothing but assembly, as ELF has
    /// its relocations.
    #[cfg(target_os = "linux")]
    pub mod symbols {
        use std::arch::naked_asm;

        extern "C" fn sym_target(a: u64) -> u64 {
            a * 3 + 1
        }
        static SYM_DATA: u64 = 1000;
        #[unsafe(naked)]
        pub extern "C" fn naked_sym(a: u64) -> u64 {
            naked_asm!("jmp {f}", f = sym sym_target)
        }
        #[unsafe(naked)]
        pub extern "C" fn naked_sym_call(a: u64) -> u64 {
            naked_asm!("push rbp", "call {f}", "add rax, 500", "pop rbp", "ret", f = sym sym_target)
        }
        #[unsafe(naked)]
        pub extern "C" fn naked_sym_data(a: u64) -> u64 {
            naked_asm!("mov rax, [rip + {d}]", "add rax, rdi", "ret", d = sym SYM_DATA)
        }
        pub fn naked_sym_operands() -> (u64, u64, u64) {
            let f: extern "C" fn(u64) -> u64 = naked_sym;
            (naked_sym(5) + f(1), naked_sym_call(2), naked_sym_data(3))
        }
    }
}

/// What `x86` has, in AArch64 assembly.
#[cfg(target_arch = "aarch64")]
pub mod arm {
    use std::arch::{asm, naked_asm};

    #[unsafe(naked)]
    pub extern "C" fn naked_add(a: u64, b: u64) -> u64 {
        naked_asm!("add x0, x0, x1", "ret")
    }
    pub fn naked_functions() -> u64 {
        naked_add(40, 2)
    }

    pub fn asm_goto() -> u32 {
        let mut out = 0;
        for x in [0u64, 5] {
            unsafe {
                asm!("cbnz {0}, {1}", in(reg) x, label { out += 10 }, options(nostack));
            }
            out += 1;
        }
        out
    }

    pub fn asm_cfg() -> u64 {
        let mut v: u64 = 1;
        unsafe {
            asm!(
                "add {0}, {0}, #2",
                #[cfg(false)]
                "add {0}, {0}, #100",
                #[cfg(target_arch = "aarch64")]
                "add {0}, {0}, #4",
                inout(reg) v,
            );
        }
        v
    }

    #[target_feature(enable = "neon")]
    fn with_neon(a: &[u32; 8]) -> u32 {
        a.iter().map(|x| x.wrapping_mul(3)).sum()
    }
    #[target_feature(enable = "neon")]
    fn calls_safely(a: &[u32; 8]) -> u32 {
        let f: unsafe fn(&[u32; 8]) -> u32 = with_neon;
        with_neon(a) + (|| with_neon(a))() + unsafe { f(a) }
    }
    pub fn target_feature_11() -> u32 {
        let a = [1, 2, 3, 4, 5, 6, 7, 8];
        if std::arch::is_aarch64_feature_detected!("neon") { unsafe { calls_safely(&a) } } else { 324 }
    }

    #[unsafe(naked)]
    pub extern "C" fn naked_const(a: u64) -> u64 {
        naked_asm!("add x0, x0, #{k}", "ret", k = const NAKED_K)
    }
    #[unsafe(naked)]
    pub extern "C" fn naked_float(a: f64, b: f64) -> f64 {
        naked_asm!("fadd d0, d0, d1", "ret")
    }
    const NAKED_K: u64 = 5 + 2;
    pub fn naked_more() -> (u64, f64, u64) {
        let f: extern "C" fn(u64) -> u64 = naked_const;
        (naked_const(10), naked_float(1.5, 2.25), f(1))
    }

    pub fn asm_goto_more() -> Vec<u32> {
        let mut out = Vec::new();
        for x in 0u64..4 {
            let hit = 0u32;
            unsafe {
                asm!(
                    "cmp {x}, #1", "b.eq {one}", "cmp {x}, #2", "b.eq {two}",
                    x = in(reg) x,
                    one = label { out.push(100) },
                    two = label { out.push(200); continue },
                );
            }
            out.push(hit);
        }
        for x in 0u64..3 {
            let v = 'found: {
                unsafe {
                    asm!("cbnz {0}, {1}", "b {2}", in(reg) x,
                         label { break 'found x as u32 * 3 }, label { break 'found 77 },
                         options(noreturn));
                }
                99
            };
            out.push(v);
        }
        out
    }

    /// A symbol in the assembly of a function that is nothing but assembly, as ELF has
    /// its relocations.
    #[cfg(target_os = "linux")]
    pub mod symbols {
        use std::arch::naked_asm;

        extern "C" fn sym_target(a: u64) -> u64 {
            a * 3 + 1
        }
        extern "C" fn sym_other(a: u32) -> u64 {
            a as u64 + 500
        }
        static SYM_DATA: u64 = 1000;
        #[unsafe(naked)]
        pub extern "C" fn naked_sym(a: u64) -> u64 {
            naked_asm!("b {f}", f = sym sym_target)
        }
        #[unsafe(naked)]
        pub extern "C" fn naked_sym_call(a: u32) -> u64 {
            naked_asm!(
                "stp x29, x30, [sp, #-16]!",
                "bl {f}",
                "bl {g}",
                "ldp x29, x30, [sp], #16",
                "ret",
                f = sym sym_other,
                g = sym sym_target,
            )
        }
        #[unsafe(naked)]
        pub extern "C" fn naked_sym_data(a: u64) -> u64 {
            naked_asm!(
                "adrp x1, {d}",
                "ldr x1, [x1, :lo12:{d}]",
                "add x0, x0, x1",
                "ret",
                d = sym SYM_DATA,
            )
        }
        pub fn naked_sym_operands() -> (u64, u64, u64) {
            let f: extern "C" fn(u64) -> u64 = naked_sym;
            (naked_sym(5) + f(1), naked_sym_call(2), naked_sym_data(3))
        }
    }

    // Sixteen bytes aligned to sixteen that the integer registers left do not hold: on the
    // stack whole, and the registers left are out of use for what follows.
    #[repr(C, align(16))]
    #[derive(Clone, Copy)]
    pub struct Aligned {
        pub a: u64,
        pub b: u32,
    }
    pub extern "C" fn wide_late(
        a: u64, b: u64, c: u64, d: u64, e: u64, f: u64, g: u64, x: u128, h: u64, y: i128,
    ) -> u128 {
        let small = a + 2 * b + 3 * c + 4 * d + 5 * e + 6 * f + 7 * g + 8 * h;
        x.wrapping_mul(3).wrapping_add(small as u128).wrapping_sub(y as u128)
    }
    pub extern "C" fn wide_fits(a: u64, b: u64, c: u64, d: u64, e: u64, x: u128, h: u64) -> u128 {
        x ^ ((a + b + c + d + e) as u128) << 64 ^ h as u128
    }
    pub extern "C" fn aligned_late(
        a: u64, b: u64, c: u64, d: u64, e: u64, f: u64, g: u64, x: Aligned, h: u64, y: Aligned,
    ) -> u64 {
        a + b + c + d + e + f + g + x.a * 10 + x.b as u64 * 100 + h * 1000 + y.a * 7 + y.b as u64
    }
    pub fn wide_stack() -> (u128, u128, u64, u128) {
        let wide: extern "C" fn(u64, u64, u64, u64, u64, u64, u64, u128, u64, i128) -> u128 =
            wide_late;
        let (x, y) = (Aligned { a: 3, b: 4 }, Aligned { a: 5, b: 6 });
        (
            wide_late(1, 2, 3, 4, 5, 6, 7, 1 << 90 | 9, 8, -5),
            wide(7, 6, 5, 4, 3, 2, 1, u128::MAX - 2, 11, i128::MIN + 3),
            aligned_late(1, 2, 3, 4, 5, 6, 7, x, 8, y),
            wide_fits(1, 2, 3, 4, 5, 1 << 100 | 1 << 20, 6),
        )
    }

    // Variable arguments of 128 bits, which come in two registers that start at an even one
    // or in sixteen bytes of the stack at a multiple of sixteen.
    #[inline(never)]
    fn stir(total: u128, at: u32) -> u128 {
        std::hint::black_box(total.rotate_left(at % 7) ^ at as u128)
    }
    #[cfg(feature = "nightly")]
    pub unsafe extern "C" fn va_wide(count: u32, mut args: ...) -> u128 {
        let mut total = 0u128;
        for at in 0..count {
            let next = match at % 4 {
                0 => unsafe { args.next_arg::<u128>() },
                1 => unsafe { args.next_arg::<u64>() as u128 },
                2 => unsafe { args.next_arg::<f64>() as u128 },
                _ => unsafe { args.next_arg::<i128>() as u128 },
            };
            // The call has the function keep registers of its caller's.
            total = stir(total.wrapping_mul(31).wrapping_add(next), at);
        }
        total
    }
    /// Named parameters that the registers do not hold: the variable ones follow them on
    /// the stack.
    pub unsafe extern "C" fn va_named(
        a: u64, b: u64, c: u64, d: u64, e: u64, f: u64, g: u64, h: u64, i: u64,
        p: f64, q: f64, r: f64, s: f64, t: f64, u: f64, v: f64, w: f64, x: f32,
        wide: u128, count: u32, mut args: ...
    ) -> f64 {
        let mut total = (a + 2 * b + 3 * c + 4 * d + 5 * e + 6 * f + 7 * g + 8 * h + 9 * i) as f64
            + p + q * 2.0 + r * 3.0 + s * 4.0 + t * 5.0 + u * 6.0 + v * 7.0 + w * 8.0
            + x as f64 * 9.0 + (wide >> 64) as f64 + wide as u64 as f64;
        for at in 0..count {
            total = stir(at as u128, at) as f64
                + total * 2.0
                + match at % 3 {
                    0 => unsafe { args.next_arg::<i64>() as f64 },
                    1 => unsafe { args.next_arg::<f64>() },
                    _ => unsafe { args.next_arg::<u32>() as f64 },
                };
        }
        total
    }
    /// A named 128-bit parameter with one register left for it.
    #[cfg(feature = "nightly")]
    pub unsafe extern "C" fn va_named_wide(
        a: u64, b: u64, c: u64, d: u64, e: u64, f: u64, g: u64, wide: i128, mut args: ...
    ) -> i128 {
        let first = unsafe { args.next_arg::<i64>() } as i128;
        let second = unsafe { args.next_arg::<i128>() };
        let third = unsafe { args.next_arg::<f64>() } as i128;
        wide + first * 3 + second * 5 + third * 7 + (a + b + c + d + e + f + g) as i128
    }
    pub fn va_named_args() -> (f64, f64, f64) {
        let through: unsafe extern "C" fn(
            u64, u64, u64, u64, u64, u64, u64, u64, u64,
            f64, f64, f64, f64, f64, f64, f64, f64, f32,
            u128, u32, ...
        ) -> f64 = va_named;
        unsafe {
            (
                va_named(
                    1, 2, 3, 4, 5, 6, 7, 8, 9, 0.5, 1.5, 2.5, 3.5, 4.5, 5.5, 6.5, 7.5, 8.5,
                    5u128 << 64 | 6, 0,
                ),
                va_named(
                    1, 2, 3, 4, 5, 6, 7, 8, 9, 0.5, 1.5, 2.5, 3.5, 4.5, 5.5, 6.5, 7.5, 8.5,
                    5u128 << 64 | 6, 5, -4i64, 0.25f64, 70u32, 12i64, 8.0f64,
                ),
                through(
                    9, 8, 7, 6, 5, 4, 3, 2, 1, 8.5, 7.5, 6.5, 5.5, 4.5, 3.5, 2.5, 1.5, 0.5,
                    1u128 << 64 | 2, 3, i64::MIN, -0.5f64, u32::MAX,
                ),
            )
        }
    }
    #[cfg(feature = "nightly")]
    pub fn va_wide_args() -> (u128, u128, u128, i128) {
        let through: unsafe extern "C" fn(u32, ...) -> u128 = va_wide;
        unsafe {
            (
                va_wide(0),
                va_wide(4, 1u128 << 100, 5u64, 2.5f64, -3i128),
                through(
                    10, 7u128, 1u64, 0.5f64, i128::MAX, u128::MAX, 2u64, 9.75f64, -1i128,
                    3u128 << 64, u64::MAX,
                ),
                va_named_wide(1, 2, 3, 4, 5, 6, 7, -1i128 << 80, 11i64, 13i128 << 66, 2.0f64),
            )
        }
    }
}
