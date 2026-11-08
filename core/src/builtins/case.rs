//! Full Unicode case mapping for `toLowerCase` and `toUpperCase` past their
//! ASCII pass, from the tables of `quickjs-oxide-regexp`, including the final-sigma
//! rule and the multi-unit expansions.

use oxide_regexp::unicode::case::convert_case_with_limit;
use oxide_regexp::unicode::{CodeUnitBuf, MAX_STRING_LEN};

/// `String.prototype.toLowerCase` (`upper` false) or `toUpperCase` of
/// UTF-16 text, as Latin-1 bytes when every mapped unit fits in one; `None`
/// past the string length limit.
pub(crate) fn convert(text: &[u16], upper: bool) -> Option<CodeUnitBuf> {
    convert_case_with_limit(text, upper, MAX_STRING_LEN).ok()
}
