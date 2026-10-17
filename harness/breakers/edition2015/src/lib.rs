//! What the 2015 edition does differently, for the cases that are about it.

use std::panic::catch_unwind;

/// What a panic that unwound said, when it said a string.
pub fn said(payload: Box<dyn std::any::Any + Send>) -> String {
    match payload.downcast::<String>() {
        Ok(said) => *said,
        Err(payload) => payload.downcast::<&str>().map(|said| said.to_string()).unwrap_or_default(),
    }
}

/// tests/ui/fmt/format-args-capture.rs: before the 2021 edition a `panic!` of one string says
/// that string as it is, braces and all, and so does one an `assert!` makes.
#[inline(never)]
pub fn braces_stay(low: u8) -> [String; 2] {
    let alone = catch_unwind(|| panic!("{braces}"));
    let asserted = catch_unwind(|| assert!(low > 200, "{braces}"));
    [alone.map_err(said).unwrap_err(), asserted.map_err(said).unwrap_err()]
}

/// A `panic!` written by a macro of this edition, wherever it is called from.
#[macro_export]
macro_rules! panic_with_braces {
    () => {
        panic!("{braces}")
    };
}
