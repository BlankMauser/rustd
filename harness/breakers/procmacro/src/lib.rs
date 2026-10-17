//! What a proc macro gives that source text cannot.

/// The name of a number, matched against constants a macro wrote as negative literals.
pub fn name(number: i8) -> &'static str {
    const NUMBERS: (i8, i16, f32) = macros::negative_numbers!();
    const MINUS_ONE: i8 = NUMBERS.0;
    match number {
        MINUS_ONE => "minus one",
        0 => "zero",
        _ => "other",
    }
}

pub fn numbers() -> (i8, i16, f32) {
    macros::negative_numbers!()
}
