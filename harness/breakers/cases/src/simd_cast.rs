//! `simd_cast` had no lowering: every function that reached one was left to rustc. serde_json
//! gets there through zmij's `_mm_mulhi_epu16`, which widens its lanes to multiply them.

use std::arch::x86_64::*;

/// The lanes of a vector, as numbers.
fn lanes<const N: usize, T: Copy + Into<i64>>(vector: __m128i) -> [i64; N] {
    let lanes: [T; N] = unsafe { std::mem::transmute_copy(&vector) };
    lanes.map(Into::into)
}

pub fn run() -> Vec<[i64; 8]> {
    unsafe {
        let a = _mm_setr_epi16(1, -2, 300, -400, 32767, -32768, 12345, 0);
        let b = _mm_setr_epi16(-1, 7, 300, 500, 32767, -32768, -3, 9);
        // Widening unsigned and signed lanes, and narrowing the products back.
        let widened = [lanes::<8, i16>(_mm_mulhi_epu16(a, b)), lanes::<8, i16>(_mm_mulhi_epi16(a, b))];
        // Integer lanes to float lanes and back.
        let ints = _mm_setr_epi32(-7, 0, 123456789, i32::MIN);
        let floats = _mm_cvtepi32_ps(ints);
        let back = lanes::<4, i32>(_mm_cvttps_epi32(floats));
        let doubles: [f64; 2] = std::mem::transmute(_mm_cvtepi32_pd(ints));
        let mut rest = [0; 8];
        rest[..4].copy_from_slice(&back);
        rest[4] = doubles[0] as i64;
        rest[5] = doubles[1] as i64;
        // Sign and zero extension of the low lanes.
        let bytes = _mm_setr_epi8(-1, 2, -3, 4, -5, 6, -7, 8, 0, 0, 0, 0, 0, 0, 0, 0);
        vec![widened[0], widened[1], rest, lanes::<8, i16>(_mm_cvtepi8_epi16(bytes)), lanes::<8, i16>(_mm_cvtepu8_epi16(bytes))]
    }
}
