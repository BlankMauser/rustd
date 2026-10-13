//! Differential-test corpus. Every `pub fn` here is compiled by rustd and called from the
//! rustc-built `runner`; the output must match an all-rustc build.
//!
//! One module per backend feature area, so a bisected culprit names the area.

pub mod scalars {
    pub fn wide_mul(a: u128, b: u128, c: i128) -> (u128, bool, Option<i128>, Option<u128>) {
        let (product, overflow) = a.overflowing_mul(b);
        (product, overflow, c.checked_mul(a as i128), (a >> 64).checked_mul(b >> 64))
    }
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

pub mod objects {
    use std::cell::Cell;
    use std::fmt::{self, Display};

    pub trait Base { fn base(&self) -> u32; fn twice(&self) -> u32 { self.base() * 2 } }
    pub trait Named { fn name(&self) -> &'static str; }
    pub trait Sized2: Sized { fn nope(&self) -> u32 { 0 } }
    pub trait Mid: Base { fn mid(&self, x: u32) -> u32; }
    /// Two supertraits with methods, a marker in between, and a method that is not in the vtable.
    pub trait Top: Mid + Send + Named {
        fn top(&mut self) -> u64;
        fn skipped(&self) -> u32 where Self: Sized { 77 }
        fn big(&self) -> [u64; 3];
    }
    pub trait Conv<T> { fn conv(&self, x: T) -> T; }

    pub struct One(pub u32);
    pub struct Two(pub u32, pub u64);
    impl Base for One { fn base(&self) -> u32 { self.0 } }
    impl Named for One { fn name(&self) -> &'static str { "one" } }
    impl Mid for One { fn mid(&self, x: u32) -> u32 { self.0 + x } }
    impl Top for One {
        fn top(&mut self) -> u64 { self.0 += 1; self.0 as u64 }
        fn big(&self) -> [u64; 3] { [self.0 as u64, 1, 2] }
    }
    impl Base for Two { fn base(&self) -> u32 { self.0 } fn twice(&self) -> u32 { 5 } }
    impl Named for Two { fn name(&self) -> &'static str { "two" } }
    impl Mid for Two { fn mid(&self, x: u32) -> u32 { self.0 * x } }
    impl Top for Two {
        fn top(&mut self) -> u64 { self.1 += 10; self.1 }
        fn big(&self) -> [u64; 3] { [self.1, self.0 as u64, 0] }
    }
    impl Conv<u8> for One { fn conv(&self, x: u8) -> u8 { x.wrapping_add(self.0 as u8) } }
    impl Conv<u64> for One { fn conv(&self, x: u64) -> u64 { x * self.0 as u64 } }
    impl Display for One {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { write!(f, "One({})", self.0) }
    }

    /// Calls every kind of slot on an object somebody else made.
    pub fn use_top(t: &mut dyn Top) -> (u32, u32, u32, u64, u64, usize) {
        let big = t.big();
        (t.base(), t.twice(), t.mid(3), t.top(), big[0] + big[1] + big[2], t.name().len())
    }
    /// Makes the objects for somebody else to call.
    pub fn make_top(two: bool) -> Box<dyn Top> {
        if two { Box::new(Two(3, 4)) } else { Box::new(One(7)) }
    }
    pub fn make_ref(one: &One) -> &dyn Top { one }
    pub fn local(x: u32) -> u64 {
        let mut one = One(x);
        let t: &mut dyn Top = &mut one;
        t.top() + t.mid(1) as u64 + t.twice() as u64
    }
    pub fn upcast_first(t: &dyn Top) -> &dyn Mid { t }
    pub fn upcast_base(t: &dyn Top) -> &dyn Base { t }
    pub fn upcast_named(t: &dyn Top) -> &dyn Named { t }
    pub fn upcast_box(t: Box<dyn Top>) -> Box<dyn Named> { t }
    pub fn use_named(n: &dyn Named) -> usize { n.name().len() }
    pub fn generic_trait(one: &One) -> (u8, u64) {
        let a: &dyn Conv<u8> = one;
        let b: &dyn Conv<u64> = one;
        (a.conv(250), b.conv(6))
    }
    pub fn boxed_sum(xs: &[Box<dyn Base>]) -> u32 {
        let mut sum = 0;
        for x in xs { sum += x.base(); }
        sum
    }
    pub fn display(x: u32) -> String {
        let d: &dyn Display = &One(x);
        d.to_string()
    }
    pub fn show(d: &dyn Display) -> usize { d.to_string().len() }

    pub struct Loud<'a>(pub &'a Cell<u64>, pub u64);
    impl Drop for Loud<'_> {
        fn drop(&mut self) { self.0.set(self.0.get() * 10 + self.1); }
    }
    pub trait Thing { fn id(&self) -> u64; }
    impl Thing for Loud<'_> { fn id(&self) -> u64 { self.1 } }
    impl Thing for u8 { fn id(&self) -> u64 { *self as u64 } }
    pub fn make_thing<'a>(log: &'a Cell<u64>, loud: bool) -> Box<dyn Thing + 'a> {
        if loud { Box::new(Loud(log, 4)) } else { Box::new(9u8) }
    }
    pub fn drop_thing(t: Box<dyn Thing + '_>) -> u64 { t.id() }

    pub fn call_fn(f: &dyn Fn(u32, u32) -> u32, x: u32) -> u32 { f(x, 2) + f(1, x) }
    pub fn call_fn_mut(f: &mut dyn FnMut(u64) -> u64) -> u64 { f(1); f(2); f(3) }
    pub fn make_fn(k: u32) -> Box<dyn Fn(u32) -> u32> { Box::new(move |x| x * k + 1) }
    pub fn make_fn_mut(mut acc: u64) -> Box<dyn FnMut(u64) -> u64> {
        Box::new(move |x| { acc += x; acc })
    }
    pub fn make_fn_once(s: String) -> Box<dyn FnOnce(usize) -> usize> {
        Box::new(move |x| s.len() + x)
    }
    pub fn local_fn(xs: &[u32], k: u32) -> u32 {
        let f: &dyn Fn(u32) -> u32 = &|x| x + k;
        let mut sum = 0;
        for &x in xs { sum += f(x); }
        sum
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
    fn make(log: &Cell<u64>, n: u64) -> Noisy<'_> {
        Noisy(log, n)
    }
    /// Temporaries die at the end of their statement, before the next one runs.
    pub fn statement_temps(log: &Cell<u64>) -> u64 {
        let a = make(log, 1).1;
        let seen = log.get();
        make(log, 2).1;
        let _keep = make(log, 3);
        a + seen * 100 + log.get() * 10000
    }
    /// A temporary in the tail expression dies as the function returns.
    pub fn tail_temp(log: &Cell<u64>) -> u64 {
        make(log, 4).1 + log.get()
    }
    pub fn tail_temp_block(log: &Cell<u64>) -> u64 {
        let n = { make(log, 5).1 };
        n + log.get() * 10
    }
    pub fn tail_temp_call(cell: &std::cell::RefCell<Vec<u64>>, n: u64) -> usize {
        fn add(cell: &std::cell::RefCell<Vec<u64>>, n: u64) -> usize {
            let id = cell.borrow().len();
            cell.borrow_mut().push(n);
            id
        }
        fn add2(cell: &std::cell::RefCell<Vec<u64>>, n: u64) -> usize {
            cell.borrow_mut().len() + n as usize
        }
        add(cell, n) + add(cell, n + 1) + add2(cell, n) + add2(cell, n)
    }
    pub fn condition_temp(log: &Cell<u64>) -> u64 {
        if make(log, 6).1 == 6 {
            log.get() + 1
        } else {
            0
        }
    }
    pub fn match_temp(log: &Cell<u64>) -> u64 {
        match make(log, 7).1 {
            7 => log.get() + 1,
            _ => 0,
        }
    }
    pub fn reassigned(log: &Cell<u64>) {
        let mut a = make(log, 1);
        a = make(log, 2);
        let _b = make(log, 3);
        a.1 += 0;
    }
    pub fn partial_move(log: &Cell<u64>) -> Noisy<'_> {
        let p = Pair { a: make(log, 4), b: make(log, 5) };
        let _late = make(log, 6);
        p.a
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
    pub fn clears(n: usize) -> usize {
        let mut empty: Vec<Vec<Option<std::sync::Arc<str>>>> = Vec::new();
        empty.clear();
        let mut full = vec![vec![Some(std::sync::Arc::<str>::from("a")), None]; n];
        full.clear();
        full.push(vec![]);
        empty.len() + full.len()
    }
    /// A `?Sized` last field stays last, even where sorting by size would move it first.
    pub fn unsizable_tail(cell: &std::cell::RefCell<[u64; 14]>, lock: &std::sync::Mutex<(u128, u8)>) -> u64 {
        *lock.lock().unwrap() = (7, 1);
        cell.borrow_mut()[13] += 1;
        cell.borrow()[0] + cell.borrow()[13]
    }
    pub trait Named {
        fn name(&self) -> usize;
    }
    impl Named for String {
        fn name(&self) -> usize {
            self.len()
        }
    }
    pub fn shared(text: &str) -> (std::sync::Arc<dyn Named>, Box<dyn Fn() -> usize>) {
        let shared: std::sync::Arc<dyn Named> = std::sync::Arc::new(text.to_owned());
        let call = {
            let shared = std::sync::Arc::clone(&shared);
            let call: Box<dyn Fn() -> usize> = Box::new(move || shared.name());
            call
        };
        (shared, call)
    }
    pub fn hashmap() -> std::collections::HashMap<std::sync::Arc<str>, u32> {
        std::collections::HashMap::new()
    }
    pub fn hashmaps(n: usize) -> Vec<std::collections::HashMap<std::sync::Arc<str>, u32>> {
        let mut maps = vec![];
        for _ in 0..n {
            maps.push(std::collections::HashMap::new());
        }
        maps
    }
    pub fn format(a: u32, b: &str) -> String {
        format!("{a}-{b}")
    }
}

pub mod simd {
    #[cfg(target_arch = "x86_64")]
    use std::arch::x86_64::*;

    pub fn ascii(s: &str) -> bool {
        s.is_ascii()
    }
    pub fn ascii_bytes(b: &[u8]) -> bool {
        b.is_ascii()
    }
    #[cfg(target_arch = "x86_64")]
    pub fn find(hay: &[u8; 16], needle: u8) -> i32 {
        unsafe {
            let hay = _mm_loadu_si128(hay.as_ptr().cast());
            _mm_movemask_epi8(_mm_cmpeq_epi8(hay, _mm_set1_epi8(needle as i8)))
        }
    }
    #[cfg(target_arch = "x86_64")]
    #[target_feature(enable = "ssse3")]
    pub unsafe fn shuffle(a: &[u8; 16], b: &[u8; 16]) -> ([u8; 16], i32) {
        unsafe {
            let x = _mm_lddqu_si128(a.as_ptr().cast());
            let y = _mm_lddqu_si128(b.as_ptr().cast());
            let z = _mm_shuffle_epi8(x, _mm_max_epu8(y, _mm_srli_epi16::<3>(x)));
            let z = _mm_andnot_si128(_mm_slli_epi16::<1>(y), _mm_min_epi8(z, x));
            let mut out = [0; 16];
            _mm_storeu_si128(out.as_mut_ptr().cast(), z);
            (out, _mm_extract_epi16::<3>(z))
        }
    }
    #[cfg(target_arch = "x86_64")]
    pub fn mix(a: &[u8; 16], b: &[u8; 16]) -> [u8; 16] {
        unsafe {
            let x = _mm_loadu_si128(a.as_ptr().cast());
            let y = _mm_loadu_si128(b.as_ptr().cast());
            let z = _mm_xor_si128(_mm_and_si128(x, y), _mm_or_si128(x, _mm_add_epi8(x, y)));
            let mut out = [0; 16];
            _mm_storeu_si128(out.as_mut_ptr().cast(), _mm_sub_epi8(z, _mm_cmpgt_epi8(x, y)));
            out
        }
    }
}

pub mod statics {
    use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, AtomicUsize, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(5);
    static FLAG: AtomicBool = AtomicBool::new(false);
    static LOW: AtomicI32 = AtomicI32::new(0);
    static mut PLAIN: [u32; 3] = [1, 2, 3];

    pub fn bump(n: u64) -> u64 {
        COUNTER.fetch_add(n, Ordering::Relaxed);
        COUNTER.fetch_sub(1, Ordering::SeqCst);
        COUNTER.fetch_or(1 << 40, Ordering::AcqRel);
        COUNTER.load(Ordering::Acquire)
    }
    pub fn once() -> (bool, bool, bool) {
        let first = FLAG.swap(true, Ordering::SeqCst);
        let second = FLAG.compare_exchange(true, false, Ordering::SeqCst, Ordering::Relaxed);
        let third = FLAG.compare_exchange(true, false, Ordering::SeqCst, Ordering::Relaxed);
        (first, second.is_ok(), third.is_ok())
    }
    pub fn lowest(n: i32) -> i32 {
        LOW.fetch_min(n, Ordering::Relaxed);
        LOW.fetch_xor(0, Ordering::Relaxed);
        LOW.load(Ordering::Relaxed)
    }
    pub fn local(n: usize) -> usize {
        let a = AtomicUsize::new(n);
        a.store(a.load(Ordering::Relaxed) * 3, Ordering::Release);
        a.fetch_max(10, Ordering::Relaxed);
        a.fetch_and(!1, Ordering::Relaxed);
        a.into_inner()
    }
    pub fn plain(i: usize, n: u32) -> u32 {
        unsafe {
            PLAIN[i] += n;
            PLAIN[0] + PLAIN[1] + PLAIN[2]
        }
    }
}

pub mod floats {
    pub fn arith(a: f64, b: f64) -> (f64, f64, f64, f64, f64) {
        (a + b, a - b, a * b, a / b, a % b)
    }
    pub fn single(a: f32, b: f32) -> (f32, f32, bool, bool) {
        (a * b + 0.5, -a % b, a < b, a == b)
    }
    pub fn compare(a: f64, b: f64) -> u8 {
        (a < b) as u8 | ((a <= b) as u8) << 1 | ((a > b) as u8) << 2 | ((a >= b) as u8) << 3 | ((a != b) as u8) << 4
    }
    pub fn to_int(x: f64) -> (i64, u64, i32, u8, i8, u16) {
        (x as i64, x as u64, x as i32, x as u8, x as i8, x as u16)
    }
    pub fn from_int(a: i64, b: u64, c: u8, d: i16) -> (f64, f64, f32, f64, f32) {
        (a as f64, b as f64, c as f32, d as f64, b as f32)
    }
    pub fn widen(x: f32, y: f64) -> (f64, f32) {
        (x as f64, y as f32)
    }
    pub fn bits(x: f64) -> (u64, f64, bool) {
        (x.to_bits(), f64::from_bits(x.to_bits() ^ (1 << 63)), x.is_nan())
    }
    #[derive(Clone, Copy)]
    pub struct Point {
        pub x: f64,
        pub y: f64,
    }
    pub fn point(p: Point, scale: f32) -> Point {
        Point { x: p.x * scale as f64, y: -p.y }
    }
    pub fn mixed(n: u32, x: f32) -> (u32, f32) {
        (n + 1, x + n as f32)
    }
    pub fn sum(values: &[f64]) -> f64 {
        let mut total = 0.0;
        for v in values {
            total += *v;
        }
        total
    }
}

pub mod locations {
    use std::panic::Location;

    #[track_caller]
    fn here() -> &'static Location<'static> {
        Location::caller()
    }
    #[track_caller]
    fn through() -> &'static Location<'static> {
        here()
    }
    /// The same location reaches both, however deep the tracked chain is.
    pub fn tracked() -> bool {
        let (a, b) = (here(), through());
        a.file() == b.file() && !a.file().is_empty()
    }
    pub fn unwraps(x: Option<u8>, y: Result<u8, u8>) -> u8 {
        x.unwrap() + y.unwrap() + x.expect("some")
    }
    pub fn borrows(cell: &std::cell::RefCell<u8>) -> u8 {
        *cell.borrow_mut() += 1;
        *cell.borrow()
    }
}

pub mod misc {
    pub fn big_repeat(seed: u8) -> u32 {
        let table = [seed; 4096];
        let wide = [(seed as u32, 7u16); 100];
        table.iter().map(|&b| b as u32).sum::<u32>() + wide[99].0 + wide[50].1 as u32
    }
    pub enum Shape {
        Circle(u32),
        Rect(u32, u32),
    }
    pub struct Wrapper(pub u32, pub bool);
    pub fn constructors(values: &[u32]) -> (u32, u32, u32) {
        let options: Vec<Option<u32>> = values.iter().copied().map(Some).collect();
        let shapes: Vec<Shape> = values.iter().copied().map(Shape::Circle).collect();
        let wrapped = values.first().copied().map(|v| (v, true)).map(|(a, b)| Wrapper(a, b));
        let area: u32 = shapes
            .iter()
            .map(|s| match s {
                Shape::Circle(r) => 3 * r * r,
                Shape::Rect(w, h) => w * h,
            })
            .sum();
        (options.iter().flatten().sum(), area, wrapped.map_or(0, |w| w.0 + w.1 as u32))
    }
    pub fn lines(text: &str) -> (usize, usize) {
        (text.lines().count(), text.lines().map(str::len).sum())
    }
    pub fn keyword(name: &str) -> u32 {
        match name {
            "alpha" => 1,
            "beta" | "gamma" => 2,
            "" => 3,
            "alphabet" => 4,
            _ => 0,
        }
    }
    pub fn barrier(mut x: u64) -> u64 {
        #[cfg(target_arch = "x86_64")]
        unsafe {
            core::arch::asm!("/*{0}*/", inout(reg) x);
        }
        x = x.wrapping_mul(3);
        x
    }
    fn target(x: u32) -> u32 {
        x + 1
    }
    pub fn fn_item_cast(x: u32) -> (bool, u32) {
        let raw = target as *const ();
        let addr = target as usize;
        let back: fn(u32) -> u32 = unsafe { core::mem::transmute(raw) };
        (raw as usize == addr, back(x))
    }
    pub fn float_math(x: f64, y: f32) -> (f64, f64, f64, f64, f64, f32, f64, f64) {
        (x.abs(), x.sqrt(), x.floor(), x.ceil(), x.round(), y.abs().trunc(), x.copysign(-1.0), x.max(2.5).min(100.0))
    }
    fn double(x: u32) -> u32 {
        x * 2
    }
    fn triple(x: u32) -> u32 {
        x * 3
    }
    static TABLE: [fn(u32) -> u32; 2] = [double, triple];
    static LAZY: core::sync::atomic::AtomicPtr<()> = core::sync::atomic::AtomicPtr::new(double as *mut ());
    pub fn fn_statics(x: u32) -> (u32, u32, u32) {
        use core::sync::atomic::Ordering;
        let first: fn(u32) -> u32 = unsafe { core::mem::transmute(LAZY.load(Ordering::Relaxed)) };
        LAZY.store(triple as *mut (), Ordering::Relaxed);
        let second: fn(u32) -> u32 = unsafe { core::mem::transmute(LAZY.load(Ordering::Relaxed)) };
        (TABLE[0](x) + TABLE[1](x), first(x), second(x))
    }
}
