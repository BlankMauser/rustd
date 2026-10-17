// What every generated program starts with: the checksum, the trait that feeds a value of any
// generated type into it, and a few generic functions the generated code instantiates.
#![allow(warnings, arithmetic_overflow, unconditional_panic, overflowing_literals, invalid_value)]
#![allow(clippy::all)]

use std::cmp::Ordering as Cmp;
use std::collections::BTreeMap;
use std::fmt::Debug;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::mem::MaybeUninit;
use std::num::{NonZeroI64, NonZeroU8, NonZeroU32};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicU64, Ordering};

static SUM: AtomicU64 = AtomicU64::new(0);

/// Folds one word into the checksum. The order of calls is part of the result.
#[inline(never)]
pub fn feed(word: u64) {
    let sum = SUM.load(Ordering::Relaxed);
    let next = (sum ^ word).wrapping_mul(0x0000_0100_0000_01b3).rotate_left(23).wrapping_add(0x9e37_79b9_7f4a_7c15);
    SUM.store(next, Ordering::Relaxed);
}

/// Feeds every part of a value into the checksum.
pub trait Mix {
    fn mix(&self);
}

macro_rules! mix_ints {
    ($($t:ty)*) => {$(
        impl Mix for $t {
            fn mix(&self) {
                feed(*self as u64);
            }
        }
    )*};
}
mix_ints!(i8 i16 i32 i64 isize u8 u16 u32 u64 usize bool char);

impl Mix for u128 {
    fn mix(&self) {
        feed(*self as u64);
        feed((*self >> 64) as u64);
    }
}

impl Mix for i128 {
    fn mix(&self) {
        (*self as u128).mix();
    }
}

// A NaN has no bits a program may rely on, so every NaN is the same word.
impl Mix for f32 {
    fn mix(&self) {
        feed(if self.is_nan() { 0x7fc0_0000 } else { self.to_bits() as u64 });
    }
}

impl Mix for f64 {
    fn mix(&self) {
        feed(if self.is_nan() { 0x7ff8_0000_0000_0000 } else { self.to_bits() });
    }
}

impl Mix for () {
    fn mix(&self) {
        feed(0x2829);
    }
}

impl Mix for str {
    fn mix(&self) {
        feed(self.len() as u64);
        for byte in self.bytes() {
            feed(byte as u64);
        }
    }
}

impl Mix for String {
    fn mix(&self) {
        self.as_str().mix();
    }
}

impl<T: Mix + ?Sized> Mix for &T {
    fn mix(&self) {
        (**self).mix();
    }
}

impl<T: Mix + ?Sized> Mix for Box<T> {
    fn mix(&self) {
        (**self).mix();
    }
}

impl<T: Mix> Mix for [T] {
    fn mix(&self) {
        feed(self.len() as u64);
        for item in self {
            item.mix();
        }
    }
}

impl<T: Mix, const N: usize> Mix for [T; N] {
    fn mix(&self) {
        self.as_slice().mix();
    }
}

impl<T: Mix> Mix for Vec<T> {
    fn mix(&self) {
        self.as_slice().mix();
    }
}

impl<T: Mix> Mix for Option<T> {
    fn mix(&self) {
        match self {
            Some(value) => {
                feed(1);
                value.mix();
            }
            None => feed(2),
        }
    }
}

impl<T: Mix, E: Mix> Mix for Result<T, E> {
    fn mix(&self) {
        match self {
            Ok(value) => {
                feed(3);
                value.mix();
            }
            Err(error) => {
                feed(4);
                error.mix();
            }
        }
    }
}

impl Mix for Cmp {
    fn mix(&self) {
        feed(*self as i8 as u64);
    }
}

macro_rules! mix_nonzero {
    ($($t:ty)*) => {$(
        impl Mix for $t {
            fn mix(&self) {
                self.get().mix();
            }
        }
    )*};
}
mix_nonzero!(NonZeroU8 NonZeroU32 NonZeroI64);

macro_rules! mix_tuples {
    ($(($($name:ident $index:tt),+))*) => {$(
        impl<$($name: Mix),+> Mix for ($($name,)+) {
            fn mix(&self) {
                $(self.$index.mix();)+
            }
        }
    )*};
}
mix_tuples! {
    (A 0)
    (A 0, B 1)
    (A 0, B 1, C 2)
    (A 0, B 1, C 2, D 3)
    (A 0, B 1, C 2, D 3, E 4)
    (A 0, B 1, C 2, D 3, E 4, F 5)
}

pub fn id<T>(value: T) -> T {
    value
}

pub fn sel<T>(first: bool, a: T, b: T) -> T {
    if first { a } else { b }
}

pub fn pick<'a, T>(first: bool, a: &'a T, b: &'a T) -> &'a T {
    if first { a } else { b }
}

pub fn twice<T: Clone>(value: &T) -> (T, T) {
    (value.clone(), value.clone())
}

pub fn larger<T: PartialOrd>(a: T, b: T) -> T {
    if a < b { b } else { a }
}

pub fn apply<A, R>(f: impl Fn(A) -> R, arg: A) -> R {
    f(arg)
}

pub fn apply_mut<A: Clone, R>(mut f: impl FnMut(A) -> R, arg: A) -> (R, R) {
    let first = f(arg.clone());
    (first, f(arg))
}

pub fn apply_once<R>(f: impl FnOnce() -> R) -> R {
    f()
}

pub fn apply_dyn<A, R>(f: &dyn Fn(A) -> R, arg: A) -> R {
    f(arg)
}

pub fn apply_dyn_mut<A, R>(f: &mut dyn FnMut(A) -> R, arg: A) -> R {
    f(arg)
}

pub fn apply_boxed<'a, R>(f: Box<dyn FnOnce() -> R + 'a>) -> R {
    f()
}

pub fn fold_array<T: Mix, const N: usize>(items: [T; N]) -> usize {
    for item in &items {
        item.mix();
    }
    N
}

/// Reads element `index` through a raw pointer.
///
/// # Safety
/// `index` is inside the allocation `start` points into.
pub unsafe fn raw_get<T: Copy>(start: *const T, index: usize) -> T {
    unsafe { *start.add(index) }
}

/// Writes element `index` through a raw pointer.
///
/// # Safety
/// `index` is inside the allocation `start` points into.
pub unsafe fn raw_set<T>(start: *mut T, index: usize, value: T) {
    unsafe { start.add(index).write(value) }
}

/// The hash of a value under a hasher with fixed keys.
pub fn hash_of<T: Hash>(value: &T) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

/// The same bytes read as different integer types. Every field covers the whole union.
#[repr(C)]
pub union Bytes4 {
    pub word: u32,
    pub signed: i32,
    pub halves: [u16; 2],
    pub bytes: [u8; 4],
}

#[repr(C)]
pub union Bytes8 {
    pub word: u64,
    pub signed: i64,
    pub halves: [u32; 2],
    pub bytes: [u8; 8],
}

#[repr(C)]
pub union Bytes16 {
    pub word: u128,
    pub signed: i128,
    pub halves: [u64; 2],
    pub bytes: [u8; 16],
}

/// A list whose links are raw pointers to nodes the caller keeps alive.
pub struct Node<T> {
    pub value: T,
    pub next: *const Node<T>,
}

/// Feeds every value of a list into the checksum and returns its length.
///
/// # Safety
/// Every `next` is null or points to a live node.
pub unsafe fn walk<T: Mix>(mut node: *const Node<T>) -> usize {
    let mut length = 0;
    while !node.is_null() {
        let here = unsafe { &*node };
        here.value.mix();
        node = here.next;
        length += 1;
    }
    length
}

/// What a panic said, when it said it in words.
pub fn payload(caught: Box<dyn std::any::Any + Send>) {
    if let Some(text) = caught.downcast_ref::<&str>() {
        text.mix();
    } else if let Some(text) = caught.downcast_ref::<String>() {
        text.mix();
    } else {
        feed(0xdead);
    }
}

/// Runs one case from a clean checksum and prints what it added up to.
fn run_case(name: &str, case: fn()) {
    SUM.store(0, Ordering::Relaxed);
    let outcome = catch_unwind(case);
    println!("{name} {:016x} {}", SUM.load(Ordering::Relaxed), if outcome.is_ok() { "ok" } else { "panic" });
}

fn main() {
    // A panic's message goes to stdout in order with the checksums, not to stderr.
    std::panic::set_hook(Box::new(|info| println!("panic: {info}")));
    let only = std::env::args().nth(1);
    for (name, case) in CASES {
        if only.as_deref().is_none_or(|it| it == *name) {
            run_case(name, *case);
        }
    }
}
