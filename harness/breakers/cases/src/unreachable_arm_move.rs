//! Found by the fuzzer (`fuzz/found/differs-checksum-L7-230-c24.rs`): a `match` on an array
//! with an arm that would move elements out and that no value reaches, because an arm
//! before it takes every value it matches (rustc warns of an unreachable pattern). rustc
//! builds nothing for the bindings of such an arm, so it knows the array is whole where it
//! is dropped and drops it front to back. rustd tries the arms one after the other, sees a
//! way through the move, and drops the array as one that has lost elements: from the back.
//! Open: which arms rustc builds nothing for follows from the tree of tests it makes of all
//! the arms together (an arm behind an overlapping range still counts as a way through), and
//! rustd does not build that tree.

#![allow(unreachable_patterns)]

use std::cell::RefCell;
use std::hint::black_box;

type Log = RefCell<Vec<String>>;

enum Loud<'a> {
    Int(i32, &'a Log),
    Char(char, &'a Log),
}

impl Drop for Loud<'_> {
    fn drop(&mut self) {
        match self {
            Loud::Int(n, log) => log.borrow_mut().push(format!("drop {n}")),
            Loud::Char(c, log) => log.borrow_mut().push(format!("drop {c}")),
        }
    }
}

fn items(log: &Log, last: char) -> [Loud<'_>; 3] {
    black_box([Loud::Int(1, log), Loud::Char('q', log), Loud::Char(last, log)])
}

fn covered_by_binding(log: &Log, last: char) -> u8 {
    match items(log, last) {
        [.., Loud::Char(_, _)] => 1,
        [Loud::Char(..), _middle @ .., Loud::Char('Z', _)] => 2,
        _ => 3,
    }
}

fn covered_with_guard(log: &Log, last: char) -> u8 {
    match items(log, last) {
        [.., Loud::Char(_, _)] => 1,
        [_first, .., Loud::Char('Z', _)] if black_box(last) == 'Z' => 2,
        _ => 3,
    }
}

pub fn run() -> Vec<String> {
    let log = RefCell::new(Vec::new());
    for last in ['a', 'Z'] {
        for case in [covered_by_binding as fn(&Log, char) -> u8, covered_with_guard] {
            let arm = case(&log, last);
            log.borrow_mut().push(format!("arm {arm}"));
        }
    }
    log.into_inner()
}
