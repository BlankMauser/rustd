//! nu-ansi-term has a source file with CRLF line endings and compares a multi-line string
//! literal of it with formatted output. The compiler reads a source file with `\n` endings,
//! and the literal kept its `\r\n`.

const PLAIN: &str = "one
two";
const RAW: &str = r"one
two";
const BYTES: &[u8] = b"one
two";
const CONTINUED: &str = "one \
    two";

pub fn run() -> (&'static str, &'static str, &'static [u8], &'static str, String) {
    (PLAIN, RAW, BYTES, CONTINUED, format!("{}
{}", 1, 2))
}
