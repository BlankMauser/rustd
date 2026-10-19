//! One small case for each way a test's result can depend on something.
//!
//! ```
//! assert_eq!(hazards::add(1, 2), 3);
//! ```

use std::cell::Cell;
use std::sync::atomic::{AtomicU32, Ordering};

pub mod shapes;

pub fn add(a: u32, b: u32) -> u32 {
    a + b
}

pub fn banner(name: &str) -> String {
    format!("hazards {name}")
}

pub fn untested() -> u32 {
    5
}

// Function pointers in a static table.
fn double(x: u32) -> u32 {
    x * 2
}
fn triple(x: u32) -> u32 {
    x * 3
}
static TABLE: [fn(u32) -> u32; 2] = [double, triple];
pub fn apply(which: usize, x: u32) -> u32 {
    TABLE[which](x)
}

// A function pointer passed along at run time.
fn pick_op(up: bool) -> fn(u32) -> u32 {
    if up { double } else { triple }
}
pub fn through_pointer(x: u32) -> u32 {
    pick_op(false)(x)
}

pub const LIMIT: u32 = 10;
pub static GREETING: &str = "hi";
const fn width() -> usize {
    4
}
pub static BUFFER: [u8; width()] = [0; width()];

pub fn limited(x: u32) -> u32 {
    x.min(LIMIT)
}

// Shared writable state.
pub static COUNTER: AtomicU32 = AtomicU32::new(0);
pub fn bump() -> u32 {
    COUNTER.fetch_add(step(), Ordering::SeqCst)
}
fn step() -> u32 {
    1
}

thread_local! {
    pub static LOCAL: Cell<u32> = const { Cell::new(local_start()) };
}
const fn local_start() -> u32 {
    3
}

// Registration that runs before main.
pub static REGISTERED: AtomicU32 = AtomicU32::new(0);
fn registered_value() -> u32 {
    11
}
extern "C" fn register() {
    REGISTERED.store(registered_value(), Ordering::SeqCst);
}
#[used]
#[link_section = ".init_array"]
static REGISTER: extern "C" fn() = register;

// Drop glue.
pub struct Guard<'a>(pub &'a Cell<u32>);
impl Drop for Guard<'_> {
    fn drop(&mut self) {
        self.0.set(dropped_value());
    }
}
fn dropped_value() -> u32 {
    9
}
pub struct Holder<'a> {
    pub guard: Guard<'a>,
}

// A generic only tests instantiate.
pub fn largest<T: Ord + Copy>(items: &[T]) -> T {
    let mut best = items[0];
    for &item in items {
        if item > best {
            best = item;
        }
    }
    best
}

macro_rules! squared {
    ($x:expr) => {
        $x * $x
    };
}
pub fn square(x: u32) -> u32 {
    squared!(x)
}

pub struct Packet {
    pub tag: u8,
    pub len: u32,
}

pub enum Signal {
    Stop,
    Go,
}

pub type Id = u8;

#[derive(PartialEq, PartialOrd)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
}

mod first {
    pub fn pick() -> u32 {
        1
    }
}
mod second {
    #[allow(dead_code)]
    pub fn pick() -> u32 {
        2
    }
}
use first::pick;
pub fn picked() -> u32 {
    pick()
}

pub trait Describe {
    fn describe(&self) -> &'static str {
        "default"
    }
}
pub struct Plain;
impl Describe for Plain {}
pub struct Fancy;
impl Describe for Fancy {}

pub trait Rated {
    const RATE: u32;
    fn rate(&self) -> u32 {
        Self::RATE
    }
}
impl Rated for Plain {
    const RATE: u32 = 2;
}

pub fn included() -> &'static str {
    include_str!("../fixtures/included.txt")
}

pub fn read_fixture() -> String {
    std::fs::read_to_string("fixtures/read.txt").unwrap()
}

pub fn mode() -> String {
    std::env::var("HAZ_MODE").unwrap_or_else(|_| "unset".to_owned())
}

pub fn checked(x: u32) -> u32 {
    if x == 0 {
        panic!("zero");
    }
    x
}

pub fn scale(x: u32, by: u32) -> u32 {
    x * by
}

pub fn from_build() -> &'static str {
    env!("HAZ_BUILD")
}

pub fn from_macro() -> u32 {
    macros::answer!()
}

pub fn from_helper(x: u32) -> u32 {
    helper::helped(x)
}

pub fn from_helper_generic(x: u32) -> (u32, u32) {
    helper::generic_help(&x)
}

#[cfg(feature = "extra")]
pub fn extra() -> u32 {
    1
}
#[cfg(not(feature = "extra"))]
pub fn extra() -> u32 {
    0
}

#[no_mangle]
pub extern "C" fn haz_exported(x: u32) -> u32 {
    x + 1
}
extern "C" {
    #[link_name = "haz_exported"]
    fn exported_elsewhere(x: u32) -> u32;
}
pub fn through_symbol(x: u32) -> u32 {
    unsafe { exported_elsewhere(x) }
}

pub struct Unmeasured {
    pub first: u8,
}

pub fn with_closure(x: u32) -> u32 {
    let add = |y: u32| y + 5;
    add(x)
}

pub fn now_is_later() -> bool {
    std::time::SystemTime::now() > std::time::UNIX_EPOCH
}

#[cfg(test)]
fn test_only() -> u32 {
    21
}

#[cfg(not(test))]
pub fn which_build() -> &'static str {
    "normal"
}
#[cfg(test)]
pub fn which_build() -> &'static str {
    "test"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adds() {
        assert_eq!(add(2, 2), 4);
    }
    #[test]
    fn banner_names() {
        assert_eq!(banner("x"), "hazards x");
    }
    #[test]
    fn table_of_pointers() {
        assert_eq!(apply(1, 2), 6);
    }
    #[test]
    fn pointer_at_run_time() {
        assert_eq!(through_pointer(2), 6);
    }
    #[test]
    fn constant() {
        assert_eq!(limited(50), 10);
    }
    #[test]
    fn greeting() {
        assert_eq!(GREETING, "hi");
    }
    #[test]
    fn buffer_width() {
        assert_eq!(BUFFER.len(), 4);
    }
    #[test]
    fn counter_one() {
        bump();
        assert!(COUNTER.load(Ordering::SeqCst) <= 2);
    }
    #[test]
    fn counter_two() {
        bump();
        assert!(COUNTER.load(Ordering::SeqCst) <= 2);
    }
    #[test]
    fn local() {
        assert_eq!(LOCAL.with(Cell::get), 3);
    }
    #[test]
    fn registered() {
        assert_eq!(REGISTERED.load(Ordering::SeqCst), 11);
    }
    #[test]
    fn drops() {
        let cell = Cell::new(0);
        drop(Guard(&cell));
        assert_eq!(cell.get(), 9);
    }
    #[test]
    fn drops_inside() {
        let cell = Cell::new(0);
        {
            let _holder = Holder { guard: Guard(&cell) };
        }
        assert_eq!(cell.get(), 9);
    }
    #[test]
    fn drops_boxed_dyn() {
        let cell = Cell::new(0);
        let boxed: Box<dyn std::any::Any + '_> = unsafe {
            std::mem::transmute::<Box<Guard<'_>>, Box<Guard<'static>>>(Box::new(Guard(&cell)))
        };
        drop(boxed);
        assert_eq!(cell.get(), 9);
    }
    #[test]
    fn generic_in_tests_only() {
        assert_eq!(largest(&[1u8, 7, 3]), 7);
    }
    #[test]
    fn macro_made() {
        assert_eq!(square(3), 9);
    }
    #[test]
    fn layout() {
        assert_eq!(std::mem::size_of::<Packet>(), 8);
    }
    #[test]
    fn discriminant() {
        assert_eq!(Signal::Go as u32, 1);
        assert_eq!(Signal::Stop as u32, 0);
    }
    #[test]
    fn alias() {
        assert_eq!(Id::MAX as u32, 255);
    }
    #[test]
    fn derived_order() {
        assert!(Version { major: 1, minor: 5 } < Version { major: 2, minor: 0 });
    }
    #[test]
    fn resolved_name() {
        assert_eq!(picked(), 1);
    }
    #[test]
    fn selected_method() {
        assert_eq!(Plain.describe(), "default");
    }
    #[test]
    fn selected_method_fancy() {
        assert_eq!(Fancy.describe(), "default");
    }
    #[test]
    fn associated_const() {
        assert_eq!(Plain.rate(), 2);
    }
    #[test]
    fn included_file() {
        assert_eq!(included(), "hello\n");
    }
    #[test]
    fn file_at_run_time() {
        assert_eq!(read_fixture(), "runtime\n");
    }
    #[test]
    fn environment() {
        assert_eq!(mode(), "unset");
    }
    #[test]
    #[should_panic(expected = "zero")]
    fn panics() {
        checked(0);
    }
    #[test]
    #[ignore]
    fn ignored() {
        assert_eq!(add(1, 1), 3);
    }
    #[test]
    fn scaled() {
        assert_eq!(scale(2, 3), 6);
    }
    #[test]
    fn build_script() {
        assert_eq!(from_build(), "7");
    }
    #[test]
    fn proc_macro() {
        assert_eq!(from_macro(), 42);
    }
    #[test]
    fn dependency() {
        assert_eq!(from_helper(1), 101);
    }
    #[test]
    fn dependency_generic() {
        assert_eq!(from_helper_generic(4), (4, 4));
    }
    #[test]
    fn feature() {
        assert_eq!(extra(), 0);
    }
    #[test]
    fn exported_symbol() {
        assert_eq!(through_symbol(1), 2);
    }
    #[test]
    fn closure() {
        assert_eq!(with_closure(1), 6);
    }
    #[test]
    fn clock() {
        assert!(now_is_later());
    }
    #[test]
    fn only_in_tests() {
        assert_eq!(test_only(), 21);
    }
    #[test]
    fn cfg_test() {
        assert_eq!(which_build(), "test");
    }
    #[test]
    fn dynamic() {
        assert_eq!(shapes::total(&shapes::all()), 4 + 3);
    }
    #[test]
    fn dynamic_one() {
        assert_eq!(shapes::first_area(), 4);
    }
    #[test]
    fn independent() {
        assert_eq!(1 + 1, 2);
    }
}
