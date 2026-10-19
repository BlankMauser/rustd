//! A landing pad is reached with what was thrown, drops what the frame still holds, and
//! carries on unwinding with it. The drops are calls, so what was thrown has to be kept
//! through them: in the pad of a body, in the pad that drops the fields of a value whose
//! `Drop` unwound, in the ones that drop the fields after one whose drop unwound, and in the
//! one that writes to a place whose old value unwound when dropped. Whoever catches it gets
//! the same payload, after every drop ran once and in order.

use std::cell::RefCell;
use std::panic::{AssertUnwindSafe, catch_unwind, panic_any};

thread_local! {
    static LOG: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

fn log(it: String) {
    LOG.with(|log| log.borrow_mut().push(it));
}

struct Noisy(&'static str);

impl Drop for Noisy {
    fn drop(&mut self) {
        log(format!("drop {}", self.0));
    }
}

/// Throws its number when dropped, if it is not zero.
struct Thrower(&'static str, u64);

impl Drop for Thrower {
    fn drop(&mut self) {
        log(format!("drop {}", self.0));
        if self.1 != 0 {
            panic_any(self.1);
        }
    }
}

/// Has a `Drop` of its own that throws, and fields that are dropped all the same.
struct Outer {
    a: Noisy,
    code: u64,
    b: Noisy,
}

impl Drop for Outer {
    fn drop(&mut self) {
        log(format!("drop outer of {} and {}", self.a.0, self.b.0));
        if self.code != 0 {
            panic_any(self.code);
        }
    }
}

struct Row {
    first: Thrower,
    second: Noisy,
    third: Thrower,
    fourth: Noisy,
}

#[inline(never)]
fn throw(code: u64) {
    if code != 0 {
        panic_any(code);
    }
}

/// Calls that unwind to the one pad of the body, from different places.
#[inline(never)]
fn body_pad(at: u64) -> u64 {
    let a = Noisy("a");
    throw(if at == 1 { 0x1111_2222_3333_4444 } else { 0 });
    let b = Noisy("b");
    throw(if at == 2 { 0x5555_6666_7777_8888 } else { 0 });
    let c = (Noisy("c0"), Noisy("c1"));
    throw(if at == 3 { u64::MAX - 2 } else { 0 });
    drop(b);
    throw(if at == 4 { 4 } else { 0 });
    a.0.len() as u64 + c.0.0.len() as u64
}

#[inline(never)]
fn drop_of_its_own(code: u64) {
    let _before = Noisy("before");
    let _it = Outer { a: Noisy("oa"), code, b: Noisy("ob") };
    let _after = Noisy("after");
    log("end".to_owned());
}

#[inline(never)]
fn fields_after(first: u64, third: u64) {
    let _before = Noisy("before");
    let _row = Row {
        first: Thrower("first", first),
        second: Noisy("second"),
        third: Thrower("third", third),
        fourth: Noisy("fourth"),
    };
    log("end".to_owned());
}

#[inline(never)]
fn elements(codes: [u64; 4], middle: u64) {
    let _before = Noisy("before");
    let _all: Vec<Thrower> = codes
        .iter()
        .zip(["e0", "e1", "e2", "e3"])
        .map(|(&code, name)| Thrower(name, code))
        .collect();
    let _array = [Thrower("f0", 0), Thrower("f1", middle), Thrower("f2", 0)];
    log("end".to_owned());
}

struct Holder {
    held: Thrower,
    other: Noisy,
}

#[inline(never)]
fn written_over(code: u64) {
    let mut it = Holder { held: Thrower("old", code), other: Noisy("other") };
    it.held = Thrower("new", 0);
    log(format!("holds {} and {}", it.held.0, it.other.0));
}

/// A pad inside a loop, reached again after it was caught further in.
#[inline(never)]
fn again_and_again() -> u64 {
    let mut sum = 0;
    for round in 1..5u64 {
        let _kept = Noisy("kept");
        let got = catch_unwind(AssertUnwindSafe(|| {
            let _inner = Outer { a: Noisy("ia"), code: (round % 2) * 1000, b: Noisy("ib") };
            throw((round + 1) % 2 * round);
        }));
        sum += match got {
            Ok(()) => 0,
            Err(payload) => *payload.downcast_ref::<u64>().unwrap_or(&7),
        };
    }
    sum
}

fn caught<T: std::fmt::Debug>(name: &str, run: impl FnOnce() -> T) {
    match catch_unwind(AssertUnwindSafe(run)) {
        Ok(it) => log(format!("{name}: returned {it:?}")),
        Err(payload) => match payload.downcast_ref::<u64>() {
            Some(code) => log(format!("{name}: threw {code:#x}")),
            None => log(format!("{name}: threw something else")),
        },
    }
}

pub fn run() -> Vec<String> {
    for at in 0..5 {
        caught("body", || body_pad(at));
    }
    for code in [0, 0xabcd_ef01_2345_6789] {
        caught("own", || drop_of_its_own(code));
        caught("over", || written_over(code));
    }
    for (first, third) in [(0, 0), (0x1_0000_0001, 0), (0, 0x2_0000_0002)] {
        caught("fields", || fields_after(first, third));
    }
    for codes in [[0; 4], [9, 0, 0, 0], [0, 0, 0x7777_0000_0000_0001, 0], [0, 0, 0, 3]] {
        caught("elements", || elements(codes, 0));
    }
    caught("elements", || elements([0; 4], 0x8000_0000_0000_0005));
    caught("again", again_and_again);
    LOG.with(|log| log.take())
}
