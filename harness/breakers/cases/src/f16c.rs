//! half converts `f32` to `f16` with `_mm_cvtps_ph`, which is `llvm.x86.vcvtps2ph.128`: an
//! instruction Cranelift has no name for. It is a function of the one instruction now.

#[cfg(target_arch = "x86_64")]
pub fn run() -> [u16; 4] {
    use std::arch::x86_64::*;
    if !is_x86_feature_detected!("f16c") {
        return [0; 4];
    }
    #[target_feature(enable = "f16c")]
    unsafe fn halves(of: [f32; 4]) -> [u16; 4] {
        unsafe {
            let halves = _mm_cvtps_ph::<_MM_FROUND_TO_NEAREST_INT>(_mm_loadu_ps(of.as_ptr()));
            let mut out = [0u16; 8];
            _mm_storeu_si128(out.as_mut_ptr().cast(), halves);
            [out[0], out[1], out[2], out[3]]
        }
    }
    unsafe { halves([1.0, -2.5, 65504.0, 1e-7]) }
}

#[cfg(not(target_arch = "x86_64"))]
pub fn run() -> [u16; 4] {
    [0; 4]
}
