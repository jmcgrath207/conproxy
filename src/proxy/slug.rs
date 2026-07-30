//! Filename-safe slugification for cache entry queries.
//!
//! Used by the `distill` feature to convert arbitrary user query text into
//! safe filenames. Lowercases, strips non-alphanumeric characters, collapses
//! runs of separators, and trims edges.

/// Slugify a query string for use as a filename.
///
/// - Lowercases the input
/// - Replaces any non-alphanumeric ASCII character with `-`
/// - Collapses runs of `-` into a single `-`
/// - Trims leading/trailing `-`
/// - Returns empty string if no alphanumeric characters remain
///
/// # Examples
///
/// ```
/// use conproxy::proxy::slugify;
/// assert_eq!(slugify("Hello, World!"), "hello-world");
/// assert_eq!(slugify("  multi   space  "), "multi-space");
/// assert_eq!(slugify("foo/bar\\baz"), "foo-bar-baz");
/// assert_eq!(slugify(""), "");
/// assert_eq!(slugify("!@#$%"), "");
/// ```
pub fn slugify(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut last_was_sep = true; // treat start as separator so we don't emit leading '-'
    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_was_sep = false;
        } else if !last_was_sep {
            out.push('-');
            last_was_sep = true;
        }
    }
    // Strip trailing '-' if present
    if out.ends_with('-') {
        out.pop();
    }
    out
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::panic
)]
mod tests {
    use super::*;

    #[test]
    fn test_slugify_basic() {
        assert_eq!(slugify("Hello, World!"), "hello-world");
    }

    #[test]
    fn test_slugify_whitespace_runs() {
        assert_eq!(slugify("  multi   space  "), "multi-space");
    }

    #[test]
    fn test_slugify_slashes_and_punctuation() {
        assert_eq!(slugify("foo/bar\\baz"), "foo-bar-baz");
    }

    #[test]
    fn test_slugify_empty() {
        assert_eq!(slugify(""), "");
    }

    #[test]
    fn test_slugify_only_punctuation() {
        assert_eq!(slugify("!@#$%"), "");
    }

    #[test]
    fn test_slugify_unicode_stripped() {
        // Non-ASCII alphanumerics like accented chars are not ASCII alphanumeric,
        // so they become separators. Result is still safe.
        assert_eq!(slugify("café au lait"), "caf-au-lait");
    }

    #[test]
    fn test_slugify_preserves_numbers() {
        assert_eq!(slugify("test 123 foo"), "test-123-foo");
    }

    #[test]
    fn test_slugify_no_trailing_separator() {
        assert_eq!(slugify("trailing/"), "trailing");
        assert_eq!(slugify("trailing   "), "trailing");
    }

    #[test]
    fn test_slugify_no_leading_separator() {
        assert_eq!(slugify("/leading"), "leading");
        assert_eq!(slugify("   leading"), "leading");
    }

    #[test]
    fn test_slugify_already_clean() {
        assert_eq!(slugify("already-clean-slug"), "already-clean-slug");
    }
}
