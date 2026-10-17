//! An `$e:expr` that a `macro_rules!` macro hands to a procedural macro is a group without
//! delimiters to rustc, also when the expression is one token. rustd's expansion goes through
//! a syntax tree between the two macros and the group of a one-token expression is lost there,
//! so the procedural macro sees a bare token (const_format's `formatcp!("{}", NAME)` then
//! expands to `let local = ;`).

macro_rules! hand_on {
    ($e:expr) => {
        passes::fragment!($e)
    };
}

const NAME: &str = "name";

pub fn handed_on() -> (&'static str, i32, &'static str) {
    (hand_on!(NAME), hand_on!(1 + 2), hand_on!("text"))
}
