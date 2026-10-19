//! A destructor that panics while its frame is being unwound: rustc ends the program there
//! ("panic in a destructor during cleanup"). rustd carries on unwinding with the second
//! panic, and whoever catches it runs on.

use std::panic::{AssertUnwindSafe, catch_unwind, panic_any};
struct T(u64);
impl Drop for T {
    fn drop(&mut self) {
        if self.0 != 0 {
            panic_any(self.0);
        }
    }
}
#[inline(never)]
fn throw(code: u64) {
    if code != 0 {
        panic_any(code);
    }
}
pub fn run() -> Vec<String> {
    let got = catch_unwind(AssertUnwindSafe(|| {
        let _inner = T(5);
        throw(1);
    }));
    vec![format!("{}", got.is_err())]
}
