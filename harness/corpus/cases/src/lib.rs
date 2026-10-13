//! Differential-test corpus. Every `pub fn` here is compiled by rustd and called from the
//! rustc-built `runner`; the output must match an all-rustc build.
//!
//! One module per backend feature area, so a bisected culprit names the area.

pub mod scalars {
    pub fn arith(a: u32, b: u32) -> u32 {
        (a + b) * 3 - (a / (b | 1)) % 7
    }
    pub fn signed(a: i64, b: i64) -> i64 {
        (a - b) / 3 + (a % 5) - (b >> 2) + (a << 3)
    }
    pub fn bits(a: u16, b: u8) -> u16 {
        (a ^ 0x5555) & !(b as u16) | (a >> (b & 7))
    }
    pub fn casts(a: i8, b: u64) -> (i64, u8, i32, u16) {
        (a as i64, b as u8, a as i32 + b as i32, a as u16)
    }
    pub fn wrapping(a: u64, b: u64) -> u64 {
        a.wrapping_mul(b).wrapping_add(a.rotate_left(7)) ^ b.wrapping_sub(a)
    }
    pub fn checked(a: u32, b: u32) -> (Option<u32>, Option<u32>, u32) {
        (a.checked_add(b), a.checked_sub(b), a.saturating_sub(b))
    }
    pub fn wide(a: u128, b: u64) -> u128 {
        a.wrapping_mul(b as u128) + 1
    }
    pub fn compare(a: i32, b: i32) -> (bool, bool, bool, bool) {
        (a < b, a == b, a >= b, a != b)
    }
    pub fn counting(a: u64) -> (u32, u32, u32) {
        (a.count_ones(), a.leading_zeros(), a.trailing_zeros())
    }
}

pub mod control {
    pub fn collatz(mut n: u64) -> u32 {
        let mut steps = 0;
        while n != 1 {
            n = if n % 2 == 0 { n / 2 } else { 3 * n + 1 };
            steps += 1;
        }
        steps
    }
    pub fn classify(n: i32) -> &'static str {
        match n {
            i32::MIN..=-1 => "negative",
            0 => "zero",
            1..=9 => "small",
            _ => "large",
        }
    }
    pub fn nested(n: u32) -> u32 {
        let mut acc = 0;
        for i in 0..n {
            for j in 0..i {
                if (i + j) % 3 == 0 {
                    continue;
                }
                acc += i * j;
                if acc > 10_000 {
                    return acc;
                }
            }
        }
        acc
    }
    pub fn fib(n: u32) -> u64 {
        if n < 2 { n as u64 } else { fib(n - 1) + fib(n - 2) }
    }
}

pub mod aggregates {
    #[derive(Debug, Clone, Copy, PartialEq)]
    pub struct Small(pub u8, pub u16);
    #[derive(Debug, Clone, Copy)]
    pub struct Pair { pub a: u32, pub b: u64 }
    #[derive(Debug, Clone, Copy)]
    pub struct Big { pub a: u64, pub b: [u32; 5], pub c: i8 }
    #[derive(Debug, Clone, Copy)]
    pub struct Odd(pub u8, pub u8, pub u8);

    pub fn small(s: Small) -> Small {
        Small(s.0 + 1, s.1 * 2)
    }
    pub fn pair(p: Pair, k: u32) -> Pair {
        Pair { a: p.a ^ k, b: p.b + k as u64 }
    }
    pub fn big(b: &Big, k: u32) -> Big {
        Big { a: b.a + 1, b: [b.b[0], k, b.b[2] + k, b.b[4], b.b[1]], c: -b.c }
    }
    pub fn big_by_value(mut b: Big) -> Big {
        b.b[3] = b.a as u32;
        b
    }
    pub fn odd(o: Odd) -> Odd {
        Odd(o.2, o.0, o.1)
    }
    pub fn tuple(t: (u8, u32, u64)) -> (u64, u8, u32) {
        (t.2 + 1, t.0 + 1, t.1 + 1)
    }
    pub fn array(a: [u16; 4]) -> [u16; 4] {
        [a[3], a[2], a[1], a[0] + 1]
    }
    pub fn nested_field(p: &(Pair, Small)) -> u64 {
        p.0.b + p.1.1 as u64
    }
    pub fn mutate(p: &mut Pair) {
        p.a += 1;
        p.b *= 2;
    }
}

pub mod enums {
    #[derive(Debug, Clone, Copy, PartialEq)]
    pub enum Plain { A, B, C }
    #[derive(Debug, Clone, Copy)]
    pub enum Explicit { Low = 3, Mid = 70, High = 200 }
    #[derive(Debug, Clone, Copy)]
    pub enum Data { Unit, One(u32), Two { a: u8, b: u64 } }
    #[derive(Debug)]
    pub enum HexError { Invalid { c: char, index: usize }, OddLength, BadLength }

    pub fn plain(n: u32) -> Plain {
        match n % 3 { 0 => Plain::A, 1 => Plain::B, _ => Plain::C }
    }
    pub fn plain_back(p: Plain) -> u32 {
        match p { Plain::A => 10, Plain::B => 20, Plain::C => 30 }
    }
    pub fn explicit(e: Explicit) -> u8 {
        e as u8
    }
    pub fn data(n: u32) -> Data {
        match n { 0 => Data::Unit, 1..=9 => Data::One(n * 2), _ => Data::Two { a: n as u8, b: n as u64 * 3 } }
    }
    pub fn data_back(d: Data) -> u64 {
        match d { Data::Unit => 0, Data::One(x) => x as u64, Data::Two { a, b } => a as u64 + b }
    }
    pub fn option(n: u32) -> Option<u32> {
        if n % 2 == 0 { Some(n / 2) } else { None }
    }
    pub fn option_ref(v: &Option<u64>) -> u64 {
        match v { Some(x) => *x + 1, None => 0 }
    }
    pub fn niche_ref<'a>(flag: bool, v: &'a u32) -> Option<&'a u32> {
        if flag { Some(v) } else { None }
    }
    pub fn niche_bool(n: u8) -> Option<bool> {
        match n { 0 => None, 1 => Some(false), _ => Some(true) }
    }
    pub fn niche_bool_back(v: Option<bool>) -> u8 {
        match v { None => 0, Some(false) => 1, Some(true) => 2 }
    }
    pub fn nested_niche(c: u8, index: usize) -> Result<u8, HexError> {
        match c {
            b'a'..=b'f' => Ok(c - b'a' + 10),
            b'0'..=b'9' => Ok(c - b'0'),
            b'!' => Err(HexError::OddLength),
            b'?' => Err(HexError::BadLength),
            _ => Err(HexError::Invalid { c: c as char, index }),
        }
    }
    pub fn nested_niche_back(r: &Result<u8, HexError>) -> u64 {
        match r {
            Ok(v) => *v as u64,
            Err(HexError::OddLength) => 1000,
            Err(HexError::BadLength) => 2000,
            Err(HexError::Invalid { c, index }) => *c as u64 * 10_000 + *index as u64,
        }
    }
    pub fn result(n: i32) -> Result<u32, i64> {
        if n >= 0 { Ok(n as u32) } else { Err(n as i64 * 2) }
    }
    pub fn ordering(a: u32, b: u32) -> core::cmp::Ordering {
        a.cmp(&b)
    }
}

pub mod slices {
    pub fn sum(xs: &[u32]) -> u64 {
        let mut total = 0;
        let mut i = 0;
        while i < xs.len() {
            total += xs[i] as u64;
            i += 1;
        }
        total
    }
    pub fn sum_iter(xs: &[u32]) -> u64 {
        let mut total = 0;
        for x in xs {
            total += *x as u64;
        }
        total
    }
    pub fn first_last(xs: &[u8]) -> Option<(u8, u8)> {
        match xs {
            [] => None,
            [only] => Some((*only, *only)),
            [first, .., last] => Some((*first, *last)),
        }
    }
    pub fn reverse(xs: &mut [u16]) {
        let n = xs.len();
        let mut i = 0;
        while i < n / 2 {
            let tmp = xs[i];
            xs[i] = xs[n - 1 - i];
            xs[n - 1 - i] = tmp;
            i += 1;
        }
    }
    pub fn tail(xs: &[u32]) -> &[u32] {
        &xs[1..]
    }
    pub fn window(xs: &[u32], from: usize, to: usize) -> u64 {
        sum(&xs[from..to])
    }
    pub fn str_len(s: &str) -> usize {
        s.len()
    }
    pub fn bytes_hash(s: &str) -> u32 {
        let mut hash = 5381u32;
        for b in s.as_bytes() {
            hash = hash.wrapping_mul(33) ^ *b as u32;
        }
        hash
    }
    pub fn literal(n: u8) -> &'static str {
        if n == 0 { "zero" } else { "nonzero" }
    }
    pub fn array_slice(a: &[u32; 6]) -> u64 {
        sum(a) + sum(&a[2..4])
    }
    pub fn get(xs: &[u64], i: usize) -> Option<u64> {
        xs.get(i).copied()
    }
    pub fn is_ascii_digit_run(s: &[u8]) -> usize {
        s.iter().take_while(|b| b.is_ascii_digit()).count()
    }
}

pub mod pointers {
    pub struct Cursor { pub start: *const u8, pub end: *const u8 }

    /// Ordering on raw pointers: RA used to lower this to a self-recursive `PartialOrd::lt`.
    pub fn peek(c: &Cursor) -> Option<u8> {
        if c.start < c.end { Some(unsafe { *c.start }) } else { None }
    }
    pub fn compare(a: *const u32, b: *const u32) -> (bool, bool, bool, bool) {
        (a < b, a >= b, a == b, a != b)
    }
    pub fn advance(c: &mut Cursor, n: usize) -> usize {
        c.start = unsafe { c.start.add(n) };
        unsafe { c.end.offset_from(c.start) as usize }
    }
    pub fn write(p: *mut u64, v: u64) -> u64 {
        unsafe {
            let old = *p;
            *p = v;
            old
        }
    }
}

pub mod consts {
    pub const LIMIT: u32 = 40 + 2;
    pub static TABLE: [u8; 8] = [1, 1, 2, 3, 5, 8, 13, 21];
    const NAMES: [&str; 3] = ["alpha", "beta", "gamma"];
    pub struct Config { pub scale: u32, pub name: &'static str }
    const CONFIG: Config = Config { scale: 9, name: "cfg" };

    pub fn limit(n: u32) -> u32 {
        n.min(LIMIT)
    }
    pub fn table(i: usize) -> u8 {
        TABLE[i % TABLE.len()]
    }
    pub fn name(i: usize) -> &'static str {
        NAMES[i % 3]
    }
    pub fn config(n: u32) -> (u32, &'static str) {
        (n * CONFIG.scale, CONFIG.name)
    }
    pub fn local_array(i: usize) -> u32 {
        let squares = [0u32, 1, 4, 9, 16, 25];
        squares[i % 6]
    }
    pub fn repeat(v: u16) -> [u16; 5] {
        [v; 5]
    }
}

pub mod generics {
    fn pick<T>(first: bool, a: T, b: T) -> T {
        if first { a } else { b }
    }
    fn swap<A: Copy, B: Copy>(pair: (A, B)) -> (B, A) {
        (pair.1, pair.0)
    }
    #[inline]
    fn add_one(x: u32) -> u32 {
        x + 1
    }
    pub struct Counter { n: u32 }
    impl Counter {
        pub fn new(n: u32) -> Counter { Counter { n } }
        pub fn bump(&mut self, by: u32) -> u32 { self.n += by; self.n }
        pub fn get(&self) -> u32 { self.n }
    }
    pub trait Shape { fn area(&self) -> u64; fn double(&self) -> u64 { self.area() * 2 } }
    pub struct Rect(pub u32, pub u32);
    pub struct Square(pub u32);
    impl Shape for Rect { fn area(&self) -> u64 { self.0 as u64 * self.1 as u64 } }
    impl Shape for Square {
        fn area(&self) -> u64 { self.0 as u64 * self.0 as u64 }
        fn double(&self) -> u64 { 7 }
    }
    fn total<S: Shape>(s: &S) -> u64 {
        s.area() + s.double()
    }

    pub fn picks(a: u64, b: u64) -> (u64, u8, (u16, u16)) {
        (pick(true, a, b), pick(false, 1u8, 2u8), pick(a > b, (1, 2), (3, 4)))
    }
    pub fn swaps(a: u8, b: u64) -> (u64, u8) {
        swap((a, b))
    }
    pub fn inline(x: u32) -> u32 {
        add_one(add_one(x))
    }
    pub fn methods(n: u32) -> u32 {
        let mut c = Counter::new(n);
        c.bump(3);
        c.bump(4) + c.get()
    }
    pub fn statics(w: u32, h: u32) -> u64 {
        total(&Rect(w, h)) + total(&Square(w))
    }
    pub fn option_map(v: Option<u32>) -> u32 {
        v.map(|x| x * 2).unwrap_or(7)
    }
    pub fn core_generics(a: u32, b: u32) -> (u32, u32) {
        (core::cmp::max(a, b), core::mem::replace(&mut { a }, b))
    }
}

pub mod closures {
    pub fn capture(k: u32, xs: &[u32]) -> u32 {
        let add = |x: u32| x + k;
        let mut total = 0;
        for x in xs {
            total += add(*x);
        }
        total
    }
    pub fn capture_mut(n: u32) -> u32 {
        let mut count = 0;
        let mut bump = |by: u32| count += by;
        bump(n);
        bump(2);
        count
    }
    pub fn iter_chain(xs: &[u32]) -> u32 {
        xs.iter().filter(|x| **x % 2 == 1).map(|x| x * 3).sum()
    }
    pub fn fn_pointer(f: fn(u32) -> u32, x: u32) -> u32 {
        f(f(x))
    }
}

pub mod callables {
    fn double(x: u32) -> u32 { x * 2 }
    fn pair(x: u32) -> (u64, u64) { (x as u64, x as u64 + 1) }
    fn apply<F: Fn(u32) -> u32>(f: F, x: u32) -> u32 { f(x) }
    fn apply_mut<F: FnMut(u32)>(mut f: F, x: u32) { f(x); f(x + 1) }
    fn apply_once<F: FnOnce() -> [u64; 3]>(f: F) -> [u64; 3] { f() }

    pub fn reify(x: u32) -> u32 {
        let f: fn(u32) -> u32 = double;
        f(x) + apply(double, x)
    }
    pub fn reify_pair(x: u32) -> (u64, u64) {
        let f: fn(u32) -> (u64, u64) = pair;
        f(x)
    }
    pub fn generic_closure(k: u32, x: u32) -> u32 {
        apply(|v| v * k, x) + apply(&|v| v + k, x)
    }
    pub fn generic_mut(x: u32) -> u32 {
        let mut total = 0;
        apply_mut(|v| total += v, x);
        total
    }
    pub fn generic_once(a: u64) -> [u64; 3] {
        let big = [a, a + 1, a + 2];
        apply_once(move || big)
    }
    pub fn takes_fn(f: fn(u8, u8) -> u8, a: u8) -> u8 { f(a, 3) }
}

pub mod odd {
    #[derive(Clone, Copy)]
    pub struct B3(pub u8, pub u8, pub u8);
    #[derive(Clone, Copy)]
    pub struct B5(pub [u8; 5]);
    #[derive(Clone, Copy)]
    pub struct B6(pub u16, pub u16, pub u16);
    #[derive(Clone, Copy)]
    pub struct B7(pub [u8; 7]);

    pub fn rot3(b: B3) -> B3 { B3(b.1, b.2, b.0) }
    pub fn rot5(b: B5) -> B5 { B5([b.0[4], b.0[0], b.0[1], b.0[2], b.0[3]]) }
    pub fn rot6(b: B6) -> B6 { B6(b.2, b.0, b.1) }
    pub fn rot7(b: B7, x: u8) -> B7 { B7([x, b.0[0], b.0[1], b.0[2], b.0[3], b.0[4], b.0[5]]) }
    pub fn chain(b: B3) -> u32 {
        let r = rot3(rot3(b));
        r.0 as u32 * 10000 + r.1 as u32 * 100 + r.2 as u32
    }
}

pub mod dynamic {
    pub trait Speak { fn speak(&self) -> u32; }
    pub struct Dog(pub u32);
    pub struct Cat;
    impl Speak for Dog { fn speak(&self) -> u32 { self.0 } }
    impl Speak for Cat { fn speak(&self) -> u32 { 9 } }

    pub fn call(s: &dyn Speak) -> u32 {
        s.speak() + 1
    }
    pub fn make(dog: bool) -> u32 {
        let d = Dog(40);
        let s: &dyn Speak = if dog { &d } else { &Cat };
        s.speak()
    }
}

pub mod drops {
    use std::cell::Cell;

    /// Appends its digit to the log when dropped.
    pub struct Noisy<'a>(pub &'a Cell<u64>, pub u64);
    impl Drop for Noisy<'_> {
        fn drop(&mut self) {
            self.0.set(self.0.get() * 10 + self.1);
        }
    }
    pub struct Pair<'a> { a: Noisy<'a>, b: Noisy<'a> }
    pub enum Either<'a> { Left(Noisy<'a>), Right(Noisy<'a>, Noisy<'a>), Neither }

    pub fn scope(log: &Cell<u64>) {
        let _a = Noisy(log, 1);
        let _b = Noisy(log, 2);
        {
            let _c = Noisy(log, 3);
        }
    }
    pub fn moved(log: &Cell<u64>, keep: bool) -> Option<Noisy<'_>> {
        let a = Noisy(log, 1);
        let b = Noisy(log, 2);
        if keep { Some(a) } else { drop(b); None }
    }
    pub fn fields(log: &Cell<u64>) {
        let _p = Pair { a: Noisy(log, 4), b: Noisy(log, 5) };
        let _t = (Noisy(log, 6), 0u8, Noisy(log, 7));
    }
    pub fn variants(log: &Cell<u64>, which: u8) {
        let _e = match which {
            0 => Either::Left(Noisy(log, 1)),
            1 => Either::Right(Noisy(log, 2), Noisy(log, 3)),
            _ => Either::Neither,
        };
    }
    pub fn arrays(log: &Cell<u64>) {
        let _xs = [Noisy(log, 1), Noisy(log, 2), Noisy(log, 3)];
    }
    pub fn boxes(log: &Cell<u64>) {
        let _b = Box::new(Noisy(log, 8));
        let _v = vec![Noisy(log, 1), Noisy(log, 2)];
    }
    pub fn param(_n: Noisy<'_>, log: &Cell<u64>) {
        let _late = Noisy(log, 9);
    }
    pub fn looped(log: &Cell<u64>, n: u64) {
        for i in 0..n {
            let _x = Noisy(log, i + 1);
        }
    }
    pub fn strings(n: usize) -> usize {
        let s = "ab".repeat(n);
        let t = s.clone();
        let v: Vec<String> = vec![s, t];
        v.len() + v[0].len()
    }
}

pub mod heap {
    pub fn boxed(n: u64) -> Box<u64> {
        Box::new(n + 1)
    }
    pub fn vec(n: u32) -> Vec<u32> {
        let mut v = Vec::new();
        for i in 0..n {
            v.push(i * i);
        }
        v
    }
    pub fn string(n: u32) -> String {
        let mut s = String::from("n=");
        for _ in 0..n {
            s.push('x');
        }
        s
    }
    pub fn consume(v: Vec<u32>) -> usize {
        v.len()
    }
    pub fn format(a: u32, b: &str) -> String {
        format!("{a}-{b}")
    }
}
