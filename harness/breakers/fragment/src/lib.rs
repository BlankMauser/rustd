//! An `$e:expr` that a `macro_rules! hand_on {
    ($e:expr) => {
        passes::fragment!($e)
    };
}

const NAME: &str = "name";

pub fn handed_on() -> (&'static str, i32, &'static str) {
    (hand_on!(NAME), hand_on!(1 + 2), hand_on!("text"))
}
