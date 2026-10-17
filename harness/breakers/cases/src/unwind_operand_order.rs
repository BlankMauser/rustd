//! Found by the fuzzer (`fuzz/found/differs-checksum-s2-23-c24.rs`): the order of drops when
//! an operand of an array, a tuple or a call panics. The operands already made belong to the
//! expression they are operands of, and a `&temporary` made on the way lives to the end of
//! the statement, so rustc drops the operands first. rustd dropped the first operand last,
//! after the temporaries that were made later than it.

use std::cell::RefCell;
use std::panic::{AssertUnwindSafe, catch_unwind};

thread_local! {
    static DROPPED: RefCell<Vec<u32>> = const { RefCell::new(Vec::new()) };
}

struct Loud(u32);

impl Drop for Loud {
    fn drop(&mut self) {
        DROPPED.with(|it| it.borrow_mut().push(self.0));
    }
}

#[inline(never)]
fn none() -> Option<Loud> {
    None
}

#[inline(never)]
fn sum(a: &Loud, b: &Loud) -> Loud {
    Loud(a.0 + b.0)
}

fn take(_a: Loud, _b: Loud, _c: Loud, _d: Loud) {}

struct Fields {
    _a: Loud,
    _b: Loud,
    _c: Loud,
}

/// The order in which `f` dropped what it made.
fn order(f: impl FnOnce()) -> Vec<u32> {
    let _ = catch_unwind(AssertUnwindSafe(f));
    DROPPED.with(|it| it.take())
}

pub fn run() -> Vec<Vec<u32>> {
    vec![
        order(|| {
            let _items = [Loud(1), sum(&Loud(20), &Loud(30)), Loud(3), none().unwrap(), Loud(4)];
        }),
        order(|| {
            let _items = (Loud(1), sum(&Loud(20), &Loud(30)), Loud(3), none().unwrap(), Loud(4));
        }),
        order(|| take(Loud(1), sum(&Loud(20), &Loud(30)), Loud(3), none().unwrap())),
        order(|| {
            let _items = Fields { _a: Loud(1), _b: sum(&Loud(20), &Loud(30)), _c: none().unwrap() };
        }),
        order(|| {
            let _items = [Loud(1), sum(&Loud(20), &Loud(30)), Loud(3)];
        }),
    ]
}
