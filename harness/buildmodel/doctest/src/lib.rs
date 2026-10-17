/// Doubles.
///
/// ```
/// assert_eq!(app::double(2), 4);
/// ```
///
/// ```should_panic
/// assert_eq!(app::double(2), 5);
/// ```
///
/// ```no_run
/// loop {
///     app::double(2);
/// }
/// ```
///
/// ```compile_fail
/// let _: String = app::double(2);
/// ```
pub fn double(n: u32) -> u32 {
    n * 2
}
