//! stacker grows the stack when less than 100k of it is left, and its test recurses through a
//! function that keeps an array of 50k: the frame of that function has to be less than twice
//! the array. cpp_demangle's parser recurses to a depth that fits rustc's frames in 2MB.

use std::hint::black_box;

#[inline(never)]
fn recurse(depth: usize, above: usize) -> usize {
    let array = [42u8; 50000];
    let here = black_box(array.as_ptr() as usize);
    let frame = if depth == 0 { above - here } else { recurse(depth - 1, here) };
    drop(array);
    frame
}

/// How many arrays of 50k fit in one frame of `recurse`.
pub fn run() -> usize {
    recurse(1, 0) / 50000
}
