//! ring declares its C functions through a macro that writes `#[link_name = concat!(...)]`
//! over each, to prefix them with its version: the symbol is what the macros expand to, not
//! the name of the function.

macro_rules! prefix {
    () => {
        "str"
    };
}

macro_rules! prefixed {
    ($name:ident, $item:item) => {
        #[link_name = concat!(prefix!(), stringify!($name))]
        $item
    };
}

unsafe extern "C" {
    prefixed! {
        len,
        fn len(text: *const std::ffi::c_char) -> usize;
    }
}

pub fn run() -> usize {
    unsafe { len(c"seven".as_ptr()) }
}
