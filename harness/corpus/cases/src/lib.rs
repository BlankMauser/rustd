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
    /// `*b;` moves the contents out and drops them, and the box can be filled again.
    pub fn box_refilled(log: &Cell<u64>) {
        let mut b = Box::new(make(log, 1));
        *b;
        let _mid = make(log, 2);
        *b = make(log, 3);
    }
    /// What a pattern leaves behind in an enum or an array is still dropped.
    pub fn left_behind(log: &Cell<u64>) {
        let pair = Some((make(log, 1), make(log, 2)));
        match pair {
            Some((_taken, _)) => {}
            None => {}
        }
        let [_first, ..] = [make(log, 3), make(log, 4), make(log, 5)];
    }
    /// Bindings drop in the reverse of the order the pattern names them, whichever
    /// alternative matched, and those of a later parameter go first.
    pub fn binding_order(log: &Cell<u64>, first: bool) {
        let ((true, _x, _y) | (false, _y, _x)) = (first, make(log, 1), make(log, 2));
        (|_a, (Ok(_b) | Err(_b)), _c| {})(make(log, 3), Ok::<_, Noisy<'_>>(make(log, 4)), make(log, 5));
    }
    fn next<'a>(n: &Noisy<'a>) -> Noisy<'a> {
        make(n.0, n.1 + 1)
    }
    /// Before the 2024 edition the temporaries in a block's tail outlive its locals.
    pub fn tail_temporaries(log: &Cell<u64>) -> u64 {
        let a = make(log, 1);
        let b = {
            let _local = make(log, 5);
            next(&next(&a)).1
        };
        b + next(&a).1
    }
    /// An arm's bindings are dropped when the arm ends, and a guard's temporaries before the
    /// arm is entered. A guard that fails gives the value back for the next arm.
    pub fn arms(log: &Cell<u64>, pick: u64) {
        let pair = Some((make(log, 1), make(log, 2)));
        let _late = make(log, 3);
        match pair {
            Some((taken, _)) if next(&taken).1 == pick => log.set(log.get() * 10 + 7),
            Some(whole) => log.set(log.get() * 10 + whole.1.1 + 6),
            None => {}
        }
    }
    /// What `if let` binds is dropped at the end of the branch, not of the function.
    pub fn if_let(log: &Cell<u64>, some: bool) {
        let _outer = make(log, 1);
        if let Some(_inner) = some.then(|| make(log, 2)) {
            log.set(log.get() * 10 + 3);
        } else {
            log.set(log.get() * 10 + 4);
        }
        log.set(log.get() * 10 + 5);
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

/// Functions with the C calling convention, called from the rustc-built runner.
pub mod foreign {
    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct Big {
        pub a: u64,
        pub b: u64,
        pub c: u64,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct Mixed {
        pub a: f64,
        pub b: u32,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct Small {
        pub a: u8,
        pub b: u8,
        pub c: u8,
    }

    pub extern "C" fn big(x: Big, n: u64) -> u64 {
        x.a + 2 * x.b + 3 * x.c + n
    }

    pub extern "C" fn make_big(n: u64) -> Big {
        Big { a: n, b: n + 1, c: n + 2 }
    }

    pub extern "C" fn mixed(x: Mixed, y: Mixed) -> Mixed {
        Mixed { a: x.a + y.a, b: x.b * y.b }
    }

    pub extern "C" fn small(x: Small, flag: bool, y: i8) -> Small {
        Small { a: x.c, b: x.b + flag as u8, c: x.a.wrapping_add(y as u8) }
    }

    pub extern "C" fn wide(x: u128, y: i16) -> i128 {
        x as i128 - y as i128
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct Floats {
        pub a: f32,
        pub b: [f32; 2],
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct Nested {
        pub tag: u8,
        pub inner: (u16, f64),
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct Two {
        pub a: u64,
        pub b: u64,
    }

    pub extern "C" fn floats(x: Floats, scale: f32) -> Floats {
        Floats { a: x.b[1] * scale, b: [x.a, x.b[0] + scale] }
    }

    pub extern "C" fn nested(x: Nested) -> Nested {
        Nested { tag: x.tag + 1, inner: (x.inner.0 * 2, x.inner.1 / 2.0) }
    }

    /// `two` no longer fits the integer registers and goes to the stack whole; `last` still
    /// gets the one that is left.
    #[allow(clippy::too_many_arguments)]
    pub extern "C" fn spill(a: u64, b: u64, c: u64, d: u64, e: u64, two: Two, last: u64) -> Two {
        Two { a: a + b + c + d + e + two.a, b: two.b * last }
    }

    pub fn through_pointer(f: extern "C" fn(Floats, f32) -> Floats, g: extern "C" fn(u64) -> Big) -> f32 {
        let x = f(Floats { a: 1.0, b: [2.0, 3.0] }, 0.5);
        x.a + x.b[0] + x.b[1] + g(3).c as f32
    }

    pub fn call_c(text: &[u8]) -> usize {
        unsafe extern "C" {
            fn strlen(s: *const u8) -> usize;
            fn div(a: i32, b: i32) -> Div;
        }
        #[repr(C)]
        struct Div {
            quot: i32,
            rem: i32,
        }
        let d = unsafe { div(47, 5) };
        let len = unsafe { strlen(text.as_ptr()) };
        len + (d.quot * 10 + d.rem) as usize
    }
}

pub mod shapes {
    use std::sync::Arc;

    #[derive(Clone, Copy, Debug, PartialEq)]
    pub enum Kind {
        Plain,
        Word,
        Line,
    }

    #[derive(Clone)]
    pub struct ByteMap {
        pub map: [Kind; 256],
    }

    pub fn byte_map() -> ByteMap {
        let mut map = [Kind::Plain; 256];
        map[b'a' as usize] = Kind::Word;
        map[b'\n' as usize] = Kind::Line;
        ByteMap { map }.clone()
    }

    #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub struct State(pub Arc<[u8]>);

    #[derive(Clone, Debug)]
    pub struct Names(pub Arc<str>, pub Vec<Option<Arc<str>>>);

    pub fn state(bytes: &[u8]) -> State {
        State(Arc::from(bytes))
    }

    pub fn names(text: &str) -> Names {
        let name: Arc<str> = Arc::from(text);
        Names(Arc::clone(&name), vec![None, Some(name)])
    }

    /// Passes its own lifetime on to the generic iterators it calls.
    pub struct SetBits<'a> {
        pub it: core::iter::Enumerate<core::slice::Iter<'a, bool>>,
    }

    impl<'a> Iterator for SetBits<'a> {
        type Item = usize;

        fn next(&mut self) -> Option<usize> {
            self.it.by_ref().find_map(|(i, &set)| set.then_some(i))
        }
    }

    pub fn wide_pointers(a: &[u8], b: &[u8]) -> (bool, bool, bool, bool) {
        let (short, long): (*const [u8], *const [u8]) = (&a[..1], a);
        let other: *const [u8] = b;
        (short == long, short < long, long == a as *const [u8], (long < other) == (other > long))
    }

    pub fn same(a: &State, b: &State) -> (bool, bool, bool) {
        (a == b, a < b, Arc::ptr_eq(&a.0, &b.0))
    }
}

/// A unit struct is a value like any other: naming it makes one, which has to be dropped.
pub mod unit_drops {
    use std::sync::atomic::{AtomicUsize, Ordering};

    static DROPS: AtomicUsize = AtomicUsize::new(0);

    pub struct Unit;
    impl Drop for Unit {
        fn drop(&mut self) {
            DROPS.fetch_add(1, Ordering::Relaxed);
        }
    }

    impl Unit {
        fn this() -> Self {
            Self
        }
    }

    pub fn count() -> [usize; 4] {
        let mut seen = [0; 4];
        {
            Unit;
        }
        seen[0] = DROPS.load(Ordering::Relaxed);
        {
            let _ = Unit;
        }
        seen[1] = DROPS.load(Ordering::Relaxed);
        {
            let _unit = Unit;
        }
        seen[2] = DROPS.load(Ordering::Relaxed);
        Unit::this();
        seen[3] = DROPS.load(Ordering::Relaxed);
        seen
    }
}

/// Operands are read when they are evaluated, not when the operation runs.
pub fn operand_order() -> (i32, i32) {
    let x = &mut 1;
    let sum = *x + {
        *x = 2;
        1
    };
    let mut boxed: Box<i32> = Box::new(3);
    #[allow(clippy::self_assignment)]
    {
        boxed = boxed;
    }
    (sum, *boxed)
}

/// A temporary in a format argument lives until the end of the statement that prints it.
pub fn format_temporaries(x: i32) -> String {
    fn boxed(x: i32) -> Option<Box<i32>> {
        Some(Box::new(x))
    }
    format!("{} {:?}", boxed(x).unwrap(), vec![x + 1])
}

pub fn exclusive_ranges(x: u32) -> [bool; 3] {
    [matches!(x, 6..7), matches!(x, 6..=7), matches!(x, ..7)]
}

pub mod const_pointers {
    static mut COUNTER: u32 = 42;
    struct Ptr(*mut u32);
    unsafe impl Sync for Ptr {}
    static TO_STATIC: Ptr = Ptr(&raw mut COUNTER);
    static mut SLICE: &mut [isize] = &mut [1];
    type Big = [u64; 4];
    const BIG: &Big = &[13, 14, 10, 13];
    const PAIR: &(isize, &Big) = &(15, BIG);
    const NUMS: [u32; 2] = [7, 9];
    #[repr(align(4))]
    struct Aligned;
    const ZST: *const () = (&Aligned as *const Aligned as *const ()).wrapping_byte_add(2);

    fn element(i: usize) -> &'static u32 {
        &NUMS[i]
    }

    fn field() -> &'static u32 {
        &(42, 1).0
    }

    /// A static that points to another static, to memory it may write, and constants that
    /// share what they point to.
    #[allow(static_mut_refs)]
    pub fn run() -> (u32, isize, bool, u32, u32, usize, usize) {
        unsafe {
            *TO_STATIC.0 += 1;
            SLICE[0] += 1;
            let unit: &'static () = &();
            (
                COUNTER,
                SLICE[0],
                std::ptr::eq(BIG, PAIR.1),
                *element(1),
                *field(),
                ZST.addr(),
                unit as *const () as usize,
            )
        }
    }
}

pub mod promoted {
    use std::hash::{Hash, Hasher};

    trait Any {}
    impl<T> Any for T {}

    fn same(a: *const dyn Any, b: *const dyn Any) -> bool {
        a == b
    }

    fn sum() -> &'static (u8, u16) {
        &(1, 2 + 3)
    }

    #[derive(Hash)]
    enum Single {
        Only(u8),
    }

    struct Bytes(Vec<u8>);
    impl Hasher for Bytes {
        fn finish(&self) -> u64 {
            0
        }
        fn write(&mut self, bytes: &[u8]) {
            self.0.extend(bytes);
        }
    }

    /// Equal promoted constants are one object, arithmetic on literals is promoted, two impls
    /// of one trait for `String` are different functions, and an enum with one variant
    /// hashes no discriminant.
    pub fn run() -> (bool, (u8, u16), bool, bool, Vec<u8>) {
        let text = "foo".to_string();
        let mut bytes = Bytes(Vec::new());
        Single::Only(17).hash(&mut bytes);
        (same(&0u8, &0u8), *sum(), text == "foo".to_string(), text == "foo", bytes.0)
    }
}

pub mod evaluation_order {
    fn log(order: &mut Vec<u8>, tag: u8) -> u32 {
        order.push(tag);
        tag as u32
    }

    struct Pair {
        a: u32,
        b: u32,
    }

    /// The right-hand side of `+=` on integers runs first, and the base of a struct literal
    /// runs last.
    pub fn run() -> (Vec<u8>, u32, u32) {
        let mut order = Vec::new();
        let mut xs = [0u32; 4];
        xs[log(&mut order, 1) as usize] += log(&mut order, 2);
        let pair = Pair { a: log(&mut order, 3), ..Pair { a: 0, b: log(&mut order, 4) } };
        (order, xs[1], pair.a + pair.b)
    }
}

pub mod pattern_alternatives {
    /// A guard that fails is run again for the next alternative of an or-pattern.
    pub fn guard(pair: (u32, u32)) -> (u32, u32) {
        let mut guards = 0;
        let found = match pair {
            (a, _) | (_, a)
                if {
                    guards += 1;
                    a > 10
                } =>
            {
                a
            }
            _ => 0,
        };
        (found, guards)
    }

    pub fn slices(xs: [u8; 5]) -> (u8, [u8; 2], u8, u8) {
        let [a, middle @ .., d, e] = xs;
        (a, middle, d, e)
    }
}

pub mod unsized_tail {
    use std::cell::Cell;
    use std::mem::{align_of_val, size_of_val};

    pub trait Get {
        fn get(&self) -> u64;
    }
    impl Get for u64 {
        fn get(&self) -> u64 {
            *self
        }
    }
    impl Get for u8 {
        fn get(&self) -> u64 {
            *self as u64
        }
    }
    struct Noisy<'a>(u8, &'a Cell<u32>);
    impl Get for Noisy<'_> {
        fn get(&self) -> u64 {
            self.0 as u64
        }
    }
    impl Drop for Noisy<'_> {
        fn drop(&mut self) {
            self.1.set(self.1.get() * 10 + self.0 as u32);
        }
    }

    struct Tail<T: ?Sized> {
        head: u16,
        tail: T,
    }

    #[repr(C, packed(2))]
    struct Packed<T: ?Sized>(u8, std::mem::ManuallyDrop<T>);

    /// The offset of a `dyn` field depends on the alignment in its vtable.
    pub fn field(wide: u64, narrow: u8) -> (u64, u64, u64, usize, usize) {
        let a: &Tail<dyn Get> = &Tail { head: 1, tail: wide };
        let b: &Tail<dyn Get> = &Tail { head: 2, tail: narrow };
        let nested: &Tail<Tail<dyn Get>> = &Tail { head: 3, tail: Tail { head: 4, tail: wide } };
        (
            a.tail.get() + a.head as u64,
            b.tail.get() + b.head as u64,
            nested.tail.tail.get() + nested.tail.head as u64,
            size_of_val(a) * 100 + align_of_val(a),
            size_of_val(b) * 100 + align_of_val(b),
        )
    }

    pub fn packed(value: u64) -> (usize, usize, isize) {
        let p: &Packed<dyn Get> = &Packed(0, std::mem::ManuallyDrop::new(value));
        let offset = unsafe {
            (&raw const p.1).cast::<u8>().offset_from(&raw const p.0)
        };
        (size_of_val(p), align_of_val(p), offset)
    }

    pub fn drop_tail(value: u8) -> u32 {
        let log = Cell::new(0);
        let boxed: Box<Tail<dyn Get + '_>> = Box::new(Tail { head: 0, tail: Noisy(value, &log) });
        let seen = boxed.tail.get() as u32;
        drop(boxed);
        log.get() * 100 + seen
    }

    pub fn run() -> ((u64, u64, u64, usize, usize), (usize, usize, isize), u32) {
        (field(11, 5), packed(7), drop_tail(3))
    }
}

pub mod upcast_generic {
    pub trait Named<T: Default + ToString> {
        fn name(&self) -> String {
            T::default().to_string()
        }
    }
    pub trait Both<T: Default + ToString>: Named<i32> + Named<T> {}

    struct S;
    impl Named<i32> for S {}
    impl Named<bool> for S {}
    impl Both<bool> for S {}

    /// Two supertraits that differ only in their arguments have separate vtables.
    pub fn run() -> (String, String) {
        let both: &dyn Both<bool> = &S;
        let int = both as &dyn Named<i32>;
        let boolean = both as &dyn Named<bool>;
        (int.name(), boolean.name())
    }
}

pub mod packed_drop {
    use std::cell::Cell;

    thread_local!(static ODD: Cell<u32> = const { Cell::new(0) });

    struct Aligned(u16);
    impl Drop for Aligned {
        fn drop(&mut self) {
            let odd = std::hint::black_box(self as *mut Aligned as usize) % 2;
            ODD.with(|it| it.set(it.get() * 10 + odd as u32 + self.0 as u32));
        }
    }
    struct Other;
    impl Drop for Other {
        fn drop(&mut self) {}
    }
    struct Pair {
        a: Aligned,
        b: Other,
    }
    #[repr(packed)]
    struct Misalign(u8, Pair);
    #[repr(packed)]
    struct Whole(u8, Aligned);

    /// A destructor never sees a misaligned `self`, with or without a partial move.
    pub fn run() -> u32 {
        let m: ([u16; 0], Misalign) = ([], Misalign(0, Pair { a: Aligned(2), b: Other }));
        let _b = m.1.1.b;
        let w: ([u16; 0], Whole) = ([], Whole(0, Aligned(4)));
        drop(w);
        drop(m.1.1.a);
        ODD.with(|it| it.get())
    }
}

pub mod array_move {
    use std::cell::RefCell;

    struct Loud<'a>(u32, &'a RefCell<Vec<u32>>);
    impl Drop for Loud<'_> {
        fn drop(&mut self) {
            self.1.borrow_mut().push(self.0);
        }
    }

    fn take<'a>([a, ref mut b, ref rest @ .., ref c, d]: [Loud<'a>; 6]) -> u32 {
        a.0 + b.0 + rest.len() as u32 + c.0 + d.0
    }

    /// Moving the first and last element out of an array parameter leaves the middle to drop.
    pub fn run() -> (u32, Vec<u32>) {
        let log = RefCell::new(Vec::new());
        let mk = |v| Loud(v, &log);
        let sum = take([mk(1), mk(2), mk(3), mk(4), mk(5), mk(6)]);
        (sum, log.into_inner())
    }
}

/// Overflow checks on signed integers when a constant is evaluated.
pub mod const_overflow {
    const SUB: Option<i8> = (-127i8).checked_sub(2);
    const ADD: Option<i8> = 127i8.checked_add(2);
    const MUL: Option<i8> = (-127i8).checked_mul(-99);
    const FITS: Option<i8> = (-5i8).checked_mul(3);
    const WIDE: (i128, bool) = i128::MIN.overflowing_sub(1);
    const UNSIGNED: (u8, bool) = 3u8.overflowing_sub(5);
    const DIV: Option<i8> = (i8::MIN + 1).checked_div(-1);
    const REM: Option<i8> = (-7i8).checked_rem(2);
    const SHR: i8 = (-64i8) >> 3;
    const SATURATED: (i8, i8, u128, u8) = (
        100i8.saturating_add(100),
        (-100i8).saturating_sub(100),
        u128::MAX.saturating_add(1),
        3u8.saturating_sub(5),
    );

    pub fn run() -> (Option<i8>, Option<i8>, Option<i8>, Option<i8>, bool, (u8, bool)) {
        (SUB, ADD, MUL, FITS, WIDE.1, UNSIGNED)
    }

    pub fn more() -> (Option<i8>, Option<i8>, i8, (i8, i8, u128, u8)) {
        (DIV, REM, SHR, SATURATED)
    }
}

/// A panic unwinds through frames that own values, and each is dropped once.
pub mod unwind_cleanup {
    use std::cell::RefCell;
    use std::panic::{self, AssertUnwindSafe};

    struct Noisy<'a>(&'a RefCell<Vec<u32>>, u32);
    impl Drop for Noisy<'_> {
        fn drop(&mut self) {
            self.0.borrow_mut().push(self.1);
        }
    }

    #[inline(never)]
    fn inner(log: &RefCell<Vec<u32>>, fail: bool) -> u32 {
        let _a = Noisy(log, 1);
        let moved = Noisy(log, 2);
        let _b = Noisy(log, 3);
        drop(moved);
        let pair = (Noisy(log, 4), Noisy(log, 5));
        let _kept = pair.0;
        if fail {
            panic!("unwinding with {} values live", log.borrow().len());
        }
        6
    }

    #[inline(never)]
    fn outer(log: &RefCell<Vec<u32>>, fail: bool) -> u32 {
        let _o = Noisy(log, 10);
        let boxed = Box::new(Noisy(log, 11));
        let n = inner(log, fail);
        drop(boxed);
        n
    }

    struct Failing<'a>(Noisy<'a>, Noisy<'a>);
    impl Drop for Failing<'_> {
        fn drop(&mut self) {
            panic!("a destructor that fails after {} drops", (self.0).0.borrow().len());
        }
    }

    #[inline(never)]
    fn scopes(log: &RefCell<Vec<u32>>) {
        let _first = Noisy(log, 20);
        let _second = Noisy(log, 21);
        {
            let _inner = Noisy(log, 22);
            let _failing = Failing(Noisy(log, 23), Noisy(log, 24));
            let _last = Noisy(log, 25);
        }
        let _unreached = Noisy(log, 26);
    }

    /// A destructor that panics still has its fields dropped, and the scopes around it unwind
    /// innermost first.
    pub fn failing_drop() -> (Vec<u32>, bool) {
        let log = RefCell::new(Vec::new());
        let caught = panic::catch_unwind(AssertUnwindSafe(|| scopes(&log))).is_err();
        (log.into_inner(), caught)
    }

    pub fn run() -> (Vec<u32>, Vec<u32>, bool) {
        let (ok, failed) = (RefCell::new(Vec::new()), RefCell::new(Vec::new()));
        outer(&ok, false);
        let caught = panic::catch_unwind(AssertUnwindSafe(|| outer(&failed, true))).is_err();
        (ok.into_inner(), failed.into_inner(), caught)
    }
}

/// Closures that each capture one field of an enum with a single variant.
pub mod capture_enum_field {
    #[derive(Debug, PartialEq)]
    pub enum Color {
        Rgb(u8, u8, u8),
    }

    pub fn run() -> Color {
        let mut color = Color::Rgb(9, 9, 9);
        let mut red = |v| {
            let Color::Rgb(ref mut r, _, _) = color;
            *r = v;
        };
        let mut blue = |v| {
            let Color::Rgb(_, _, ref mut b) = color;
            *b = v;
        };
        red(1);
        blue(3);
        color
    }
}

/// A method call that borrows a constant uses the constant where it is, not a copy on the stack.
pub mod autoref_promoted {
    const BIG: [u8; 32 * 1024 * 1024] = [42; 32 * 1024 * 1024];
    const SMALL: [u16; 3] = [1, 2, 3];

    pub fn run() -> (usize, Option<&'static u16>, bool) {
        (BIG.len(), SMALL.last(), std::ptr::eq(SMALL.as_ptr(), SMALL.as_ptr()))
    }
}

/// `Box<dyn FnMut>` forwards its arguments to the closure as the tuple it was called with.
pub mod boxed_fn_tuple {
    fn join(key: String, val: String, n: u8) -> String {
        format!("{key}={val}/{n}")
    }

    pub fn run() -> (String, u32, String) {
        let mut seen = 0;
        let mut two: Box<dyn FnMut(String, String, u8) -> String> = Box::new(|a, b, n| {
            seen += 1;
            join(a, b, n)
        });
        let joined = two("k".to_string(), "v".to_string(), 7);
        drop(two);
        let none: Box<dyn Fn() -> String> = Box::new(|| "none".to_string());
        (joined, seen, none())
    }
}

/// `line!()`, `column!()` and `file!()` are where the outermost macro is called.
pub mod builtin_location {
    macro_rules! here {
        () => {
            (line!(), column!(), file!())
        };
    }

    macro_rules! column_of {
        ($e:expr) => {
            ($e, column!())
        };
    }

    /// A macro call passed to another macro is where it was written.
    pub fn passed() -> (u32, u32) {
        column_of!(column_of!(7).1)
    }

    pub fn run() -> ((u32, u32, &'static str), (u32, u32, &'static str), u32) {
        let direct = (line!(), column!(), file!());
        let nested = here!();
        assert_eq!(nested.0, line!() - 1, "in {}", file!());
        (direct, nested, line!() + 1)
    }
}

/// `stringify!` spaces tokens the way rustc does.
pub mod stringify_spacing {
    macro_rules! ty {
        (<$t:ty>) => {
            stringify!($t)
        };
    }
    macro_rules! expr {
        ($e:expr) => {
            stringify!($e)
        };
    }
    macro_rules! list {
        ($($e:ident),*) => {
            stringify!($($e),*)
        };
    }

    pub fn run() -> Vec<&'static str> {
        vec![
            stringify!(std::option),
            stringify!(a , b,c),
            stringify!(f(x).y.0 + #[attr] g ( 1 )),
            stringify!({ foo += 1; eprintln!("before"); 7331 }),
            stringify!(let (a, b) = &'a mut x;),
            stringify!(fn f<T: Tr>(x: Vec<T>) -> [u8; 3] {}),
            ty!(<Vec<i32>>),
            list!(a, b, c),
            stringify!(("Yeah",)),
            stringify!(1u8),
            stringify!(r#struct . r#fn),
            expr!(1 + 2),
            expr!(Point { x: 42 }),
            expr!(&a),
        ]
    }
}

/// Raw identifiers mean what the plain ones do, except to a macro matching on them.
pub mod raw_identifiers {
    #[r#repr(r#C, r#packed)]
    pub struct Packed {
        pub a: bool,
        pub b: u64,
    }

    #[derive(Debug, Clone)]
    pub struct Keyword {
        pub r#struct: u32,
    }

    macro_rules! which {
        (a) => {
            6
        };
        (r#a) => {
            7
        };
    }

    pub fn run() -> (usize, String, u32, u32) {
        let keyword = Keyword { r#struct: 3 };
        (std::mem::size_of::<Packed>(), format!("{:?}", keyword.clone()), which!(a), which!(r#a))
    }
}

/// A derive calls the trait's own methods, whatever else is in scope under those names.
pub mod derive_ufcs {
    use std::fmt;

    #[derive(Debug, Clone)]
    pub struct Named {
        pub bar: u8,
        pub baz: (),
    }

    #[derive(Debug, Clone)]
    pub struct Tuple(pub u8);

    #[derive(Debug, Clone)]
    pub struct Unit;

    pub trait Shadow {
        fn debug_struct(&self, _: &str) -> u8 {
            panic!("got into debug_struct")
        }
        fn debug_tuple(&self, _: &str) -> u8 {
            panic!("got into debug_tuple")
        }
        fn write_str(&self, _: &str) -> u8 {
            panic!("got into write_str")
        }
        fn clone(&self) -> u8 {
            panic!("got into clone")
        }
    }
    impl Shadow for &mut fmt::Formatter<'_> {}
    impl Shadow for &u8 {}
    impl Shadow for &() {}

    pub fn run() -> String {
        let named = Named { bar: 1, baz: () };
        format!("{:?} {:?} {:?}", named.clone(), Tuple(2).clone(), Unit.clone())
    }
}

/// `Clone` of the types that have no impl written anywhere.
pub mod builtin_clone {
    fn twice(n: u8) -> u8 {
        n * 2
    }

    pub fn run() -> ((String, u8), [String; 2], u8, ((), (Vec<u8>,))) {
        let pair = ("a".to_string(), 1u8);
        let array = ["b".to_string(), "c".to_string()];
        let pointer: fn(u8) -> u8 = twice;
        let nested = ((), (vec![1u8, 2],));
        (Clone::clone(&pair), array.clone(), Clone::clone(&pointer)(4), nested.clone())
    }
}

/// A guard that fails gives back what the pattern took, and `&&` drops as it goes.
pub mod guard_moves {
    use std::cell::RefCell;

    struct Noisy<'a>(&'a RefCell<Vec<u32>>, u32);

    impl Drop for Noisy<'_> {
        fn drop(&mut self) {
            self.0.borrow_mut().push(self.1);
        }
    }

    impl Noisy<'_> {
        fn yes(&self) -> bool {
            true
        }
    }

    fn failed(log: &RefCell<Vec<u32>>) -> u32 {
        match (Noisy(log, 2), Noisy(log, 3)) {
            (_x, _y) if let _z = Noisy(log, 1) && false => 0,
            (x, _) if x.1 == 7 => 1,
            (_, y) => y.1,
        }
    }

    fn chain(log: &RefCell<Vec<u32>>) -> u32 {
        if Noisy(log, 1).yes()
            && let Some(_v) = Some(Noisy(log, 5))
            && Noisy(log, 2).yes()
            && let Some(_) = Some(Noisy(log, 4)).as_ref()
        {
            log.borrow_mut().push(3);
        }
        (Noisy(log, 6).yes() || Noisy(log, 99).yes()) as u32
    }

    pub fn run() -> (u32, u32, Vec<u32>) {
        let log = RefCell::new(Vec::new());
        let (a, b) = (failed(&log), chain(&log));
        (a, b, log.into_inner())
    }
}

/// Dividing by zero and `MIN / -1` panic and unwind.
pub mod division_panics {
    use std::hint::black_box;
    use std::panic;

    pub fn run() -> (Vec<bool>, i128, u8, i64) {
        let caught = vec![
            panic::catch_unwind(|| black_box(1i32) / black_box(0)).is_err(),
            panic::catch_unwind(|| black_box(1u8) % black_box(0)).is_err(),
            panic::catch_unwind(|| black_box(i8::MIN) / black_box(-1)).is_err(),
            panic::catch_unwind(|| black_box(i64::MIN) % black_box(-1)).is_err(),
            panic::catch_unwind(|| black_box(i128::MIN) / black_box(-1)).is_err(),
            panic::catch_unwind(|| black_box(i16::MIN) / black_box(1)).is_err(),
        ];
        (caught, black_box(i128::MIN + 1) / black_box(-1), black_box(7u8) % black_box(4), black_box(-7i64) / 2)
    }
}

/// What a panic unwinds through is dropped: the captures of a closure called by value, and the
/// part of a tuple or an array that was already cloned.
pub mod unwind_owned {
    use std::panic::{self, AssertUnwindSafe};
    use std::rc::Rc;

    struct Flag<'a>(&'a mut bool);

    impl Drop for Flag<'_> {
        fn drop(&mut self) {
            *self.0 = true;
        }
    }

    fn call<T: FnOnce()>(t: T) {
        t()
    }

    struct Seventh(Rc<()>);

    impl Clone for Seventh {
        fn clone(&self) -> Self {
            if Rc::strong_count(&self.0) == 7 {
                panic!("oops");
            }
            Seventh(self.0.clone())
        }
    }

    pub fn run() -> (bool, bool, usize, usize) {
        let mut dropped = false;
        let caught = {
            let flag = Flag(&mut dropped);
            panic::catch_unwind(AssertUnwindSafe(move || {
                let inner = move || {
                    let _flag = &flag;
                    panic!()
                };
                call(inner);
            }))
            .is_err()
        };

        let counter = Rc::new(());
        let c = counter.clone();
        let _ = panic::catch_unwind(AssertUnwindSafe(move || {
            let _ = (Seventh(c.clone()), Seventh(c.clone()), Seventh(c.clone()), Seventh(c)).clone();
        }));
        let tuple = Rc::strong_count(&counter);
        let c = counter.clone();
        let _ = panic::catch_unwind(AssertUnwindSafe(move || {
            let _ = [Seventh(c.clone()), Seventh(c.clone()), Seventh(c.clone()), Seventh(c)].clone();
        }));
        (caught, dropped, tuple, Rc::strong_count(&counter))
    }
}

/// Derives on a `Copy` type copy it, and on a packed type they copy the fields out before
/// calling anything on them.
pub mod derive_copy_packed {
    use std::sync::atomic::{AtomicU32, Ordering};

    pub static CLONED: AtomicU32 = AtomicU32::new(0);
    pub static MISALIGNED: AtomicU32 = AtomicU32::new(0);

    #[derive(Copy)]
    pub struct Liar;

    impl Clone for Liar {
        fn clone(&self) -> Self {
            CLONED.fetch_add(1, Ordering::SeqCst);
            *self
        }
    }

    #[derive(Copy, Clone)]
    pub struct Innocent(pub Liar);

    #[derive(Clone)]
    pub struct Honest(pub Liar);

    #[derive(Copy, Clone, Debug, Hash, PartialOrd)]
    pub struct Aligned(pub usize);

    impl PartialEq for Aligned {
        fn eq(&self, other: &Self) -> bool {
            for it in [self, other] {
                if it as *const Aligned as usize % std::mem::align_of::<Aligned>() != 0 {
                    MISALIGNED.fetch_add(1, Ordering::SeqCst);
                }
            }
            self.0 == other.0
        }
    }

    #[repr(packed)]
    #[derive(Copy, Clone, PartialEq, PartialOrd, Debug, Hash)]
    pub struct Packed(pub Aligned, pub Aligned);

    #[derive(PartialEq)]
    #[repr(C)]
    pub struct Dealigned<T>(pub u8, pub T);

    pub fn run() -> (u32, u32, bool, bool, String, u32) {
        let _ = Innocent(Liar).clone();
        let copied = CLONED.load(Ordering::SeqCst);
        let _ = Honest(Liar).clone();
        let a = Dealigned(0, Packed(Aligned(1), Aligned(2)));
        let b = Dealigned(0, Packed(Aligned(1), Aligned(3)));
        (
            copied,
            CLONED.load(Ordering::SeqCst),
            a == a,
            a.1 < b.1,
            format!("{:?}", b.1),
            MISALIGNED.load(Ordering::SeqCst),
        )
    }
}

/// A constant whose pointer sits in a union, where its type does not show it.
pub mod union_pointer {
    use std::mem::MaybeUninit;

    type Byte = MaybeUninit<u8>;

    const unsafe fn memcpy(dst: *mut Byte, src: *const Byte, n: usize) {
        let mut i = 0;
        while i < n {
            unsafe { *dst.add(i) = *src.add(i) };
            i += 1;
        }
    }

    const COPIED: MaybeUninit<*const i32> = unsafe {
        let ptr = &42;
        let mut ptr2 = MaybeUninit::new(std::ptr::null::<i32>());
        memcpy(&mut ptr2 as *mut _ as *mut _, &ptr as *const _ as *const _, size_of::<&i32>());
        ptr2
    };
    const DIRECT: MaybeUninit<(u64, &'static u8)> = MaybeUninit::new((7, &9));

    pub fn run() -> (i32, u64, u8) {
        let direct = unsafe { DIRECT.assume_init() };
        (unsafe { COPIED.assume_init().read() }, direct.0, *direct.1)
    }
}

pub mod async_basic {
    use std::cell::RefCell;
    use std::future::Future;
    use std::pin::{Pin, pin};
    use std::rc::Rc;
    use std::task::{Context, Poll, Waker};

    type Log = Rc<RefCell<Vec<&'static str>>>;

    struct Noisy(&'static str, Log);

    impl Drop for Noisy {
        fn drop(&mut self) {
            self.1.borrow_mut().push(self.0);
        }
    }

    /// Pending the given number of times, then ready.
    struct Countdown(u32);

    impl Future for Countdown {
        type Output = u32;

        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<u32> {
            if self.0 == 0 {
                return Poll::Ready(7);
            }
            self.0 -= 1;
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    }

    fn block_on<F: Future>(future: F) -> (F::Output, u32) {
        let mut future = pin!(future);
        let mut cx = Context::from_waker(Waker::noop());
        let mut polls = 1;
        loop {
            if let Poll::Ready(value) = future.as_mut().poll(&mut cx) {
                return (value, polls);
            }
            polls += 1;
        }
    }

    async fn add(a: u32, b: u32) -> u32 {
        a + Countdown(b).await
    }

    async fn sum(values: &[u32], log: Log) -> u32 {
        let _whole = Noisy("sum", log.clone());
        let mut total = 0;
        for &value in values {
            let _each = Noisy("step", log.clone());
            total = add(total, value).await;
        }
        total
    }

    pub fn run() -> (u32, u32, (u32, u32), (u32, u32), Vec<&'static str>, Vec<&'static str>) {
        let (plain, _) = block_on(async { 5 });
        let captured = String::from("abc");
        let (moved, _) = block_on(async move { captured.len() as u32 + Countdown(2).await });
        let log = Log::default();
        let summed = block_on(sum(&[1, 2, 0], log.clone()));
        let boxed: Pin<Box<dyn Future<Output = u32>>> = Box::pin(add(1, 3));
        let dynamic = block_on(boxed);

        // Dropped before it ran, and dropped while it waits.
        let dropped = Log::default();
        let never = sum(&[1], dropped.clone());
        let guard = Noisy("captured", dropped.clone());
        let unresumed = async move {
            let _guard = guard;
        };
        drop(unresumed);
        {
            let mut waiting = pin!(never);
            let mut cx = Context::from_waker(Waker::noop());
            assert!(waiting.as_mut().poll(&mut cx).is_pending());
            dropped.borrow_mut().push("polled");
        }
        dropped.borrow_mut().push("end");
        let first = log.borrow().clone();
        let second = dropped.borrow().clone();
        (plain, moved, summed, dynamic, first, second)
    }
}

pub mod async_more {
    use std::cell::RefCell;
    use std::future::Future;
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::pin::{Pin, pin};
    use std::rc::Rc;
    use std::task::{Context, Poll, Waker};

    type Log = Rc<RefCell<Vec<String>>>;

    struct Noisy(&'static str, Log);

    impl Drop for Noisy {
        fn drop(&mut self) {
            self.1.borrow_mut().push(self.0.to_string());
        }
    }

    struct Yield(bool);

    impl Future for Yield {
        type Output = ();

        fn poll(mut self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<()> {
            if self.0 {
                return Poll::Ready(());
            }
            self.0 = true;
            Poll::Pending
        }
    }

    fn block_on<F: Future>(future: F) -> F::Output {
        let mut future = pin!(future);
        let mut cx = Context::from_waker(Waker::noop());
        loop {
            if let Poll::Ready(value) = future.as_mut().poll(&mut cx) {
                return value;
            }
        }
    }

    trait Named {
        fn name(&self) -> String;
    }

    impl Named for u8 {
        fn name(&self) -> String {
            format!("u8:{self}")
        }
    }

    impl Named for &str {
        fn name(&self) -> String {
            format!("str:{self}")
        }
    }

    async fn generic<T: Named>(value: T, log: Log) -> usize {
        let name = value.name();
        Yield(false).await;
        log.borrow_mut().push(name.clone());
        name.len()
    }

    async fn borrows(text: &mut String) -> &str {
        text.push('!');
        Yield(false).await;
        text.push('?');
        &text[1..]
    }

    async fn branches(n: u32, log: Log) -> Result<u32, String> {
        let _guard = Noisy("branches", log.clone());
        let mut seen = 0;
        loop {
            if seen == n {
                break;
            }
            let _step = Noisy("iter", log.clone());
            Yield(false).await;
            seen += 1;
            if seen == 3 {
                return Err(format!("stopped at {seen}"));
            }
        }
        match seen {
            0 => Ok(0),
            other => {
                let doubled = async { other * 2 }.await;
                Ok(doubled)
            }
        }
    }

    async fn panics(log: Log) -> u32 {
        let _outer = Noisy("panic-outer", log.clone());
        Yield(false).await;
        let _inner = Noisy("panic-inner", log.clone());
        if log.borrow().len() < 100 {
            panic!("in poll");
        }
        1
    }

    fn boxed(depth: u32) -> Pin<Box<dyn Future<Output = u32>>> {
        Box::pin(async move {
            if depth == 0 {
                return 1;
            }
            Yield(false).await;
            boxed(depth - 1).await + depth
        })
    }

    struct Counting(std::sync::atomic::AtomicU32);

    impl std::task::Wake for Counting {
        fn wake(self: std::sync::Arc<Self>) {
            self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }

    struct WakeTwice(u32);

    impl Future for WakeTwice {
        type Output = ();

        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
            if self.0 == 2 {
                return Poll::Ready(());
            }
            self.0 += 1;
            cx.waker().clone().wake();
            Poll::Pending
        }
    }

    /// A waker is a pointer to a promoted table of functions.
    fn wakes() -> u32 {
        let counting = std::sync::Arc::new(Counting(Default::default()));
        let waker = Waker::from(counting.clone());
        let mut cx = Context::from_waker(&waker);
        let mut future = pin!(async { WakeTwice(0).await });
        while future.as_mut().poll(&mut cx).is_pending() {}
        counting.0.load(std::sync::atomic::Ordering::Relaxed)
    }

    trait Greets {
        async fn name() -> String {
            "default".to_owned()
        }

        async fn greet() -> String {
            format!("hello {}", Self::name().await)
        }
    }

    struct Plain;
    struct Custom;

    impl Greets for Plain {}

    impl Greets for Custom {
        async fn name() -> String {
            Yield(false).await;
            "custom".to_owned()
        }
    }

    async fn join(log: Log) -> (usize, usize) {
        let a = generic(7u8, log.clone());
        let b = generic("xyz", log.clone());
        let second = b.await;
        (a.await, second)
    }

    pub fn run() -> Vec<String> {
        let log = Log::default();
        let mut out = Vec::new();
        out.push(format!("{:?}", block_on(join(log.clone()))));
        let mut text = String::from("ab");
        out.push(block_on(borrows(&mut text)).to_string());
        out.push(format!("{:?}", block_on(branches(0, log.clone()))));
        out.push(format!("{:?}", block_on(branches(2, log.clone()))));
        out.push(format!("{:?}", block_on(branches(5, log.clone()))));
        out.push(format!("{}", block_on(boxed(4))));
        let caught = catch_unwind(AssertUnwindSafe(|| block_on(panics(log.clone()))));
        out.push(format!("{}", caught.is_err()));
        let closure = |x: u32| async move { x + 1 };
        out.push(format!("{}", block_on(closure(4))));
        out.push(format!("{}", wakes()));
        out.push(block_on(Plain::greet()));
        out.push(block_on(Custom::greet()));
        out.extend(log.borrow().iter().cloned());
        out
    }
}
