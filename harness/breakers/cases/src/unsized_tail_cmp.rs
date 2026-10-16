//! triomphe orders `HeaderSlice<H, [T]>`, a struct that ends in a slice, by a derive. The
//! comparison of two references to it reads each as one scalar, and they are a pointer and a
//! length. Open: the function is left to rustc.

#[derive(PartialEq, PartialOrd)]
#[repr(C)]
struct HeaderSlice<H, T: ?Sized> {
    header: H,
    slice: T,
}

pub fn run() -> [bool; 4] {
    let small: &HeaderSlice<u8, [u8]> = &HeaderSlice { header: 1, slice: [1, 2] };
    let large: &HeaderSlice<u8, [u8]> = &HeaderSlice { header: 1, slice: [1, 2, 3] };
    [small <= large, small >= large, small <= small, &small < &large]
}
