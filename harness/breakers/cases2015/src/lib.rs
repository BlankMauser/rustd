//! Reductions that need the 2015 edition: what later editions no longer accept. Each is run
//! through the module of the same name in `breakers`.

#![allow(ellipsis_inclusive_range_patterns)]

/// autocfg writes inclusive range patterns with three dots.
pub mod dots_range_pattern {
    fn mangle(s: &str) -> String {
        s.chars()
            .map(|c| match c {
                'A'...'Z' | 'a'...'z' | '0'...'9' => c,
                _ => '_',
            })
            .collect()
    }

    fn class(n: u8) -> u8 {
        match n {
            0...9 => 0,
            10...99 => 1,
            _ => 2,
        }
    }

    pub fn run() -> (String, [u8; 4]) {
        (mangle("has-atomic 64!"), [class(0), class(9), class(10), class(200)])
    }
}
