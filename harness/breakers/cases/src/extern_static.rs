//! onig reads the encodings of its C library as `extern` statics. A static declared in an
//! `extern` block is defined by whatever is linked, under its own name, whoever compiles
//! the crate that declares it.

use std::ffi::{c_char, c_int};

unsafe extern "C" {
    static environ: *const *const c_char;
    #[link_name = "optind"]
    static mut NEXT_ARGUMENT: c_int;
}

pub fn run() -> (bool, c_int) {
    let has_environment = unsafe { !environ.is_null() };
    let next = unsafe { *(&raw const NEXT_ARGUMENT) };
    (has_environment, next)
}
