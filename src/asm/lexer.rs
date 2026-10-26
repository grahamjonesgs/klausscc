//! Pure text predicates for classifying one line of assembler source.
//!
//! These look only at the text: whether a line is blank, a comment, a label,
//! a data definition or the start marker, and how to split off a trailing `//`
//! comment.  Classification that needs the opcode table
//! (`LineType::classify`) is in the `classify` module.  This module has no intra-crate dependencies.

/// Strip a trailing `//` comment.
///
/// Returns the text before any `//`, without leading and trailing whitespace.
pub(crate) fn strip_comments(input: &str) -> &str {
    input.find("//").map_or(input, |location| &input[..location]).trim()
}

/// Return a trailing `//` comment.
///
/// Returns the text after the first `//`, without leading and trailing
/// whitespace, or `""` when the line has no comment.
pub(crate) fn return_comments(input: &str) -> &str {
    input.find("//").map_or("", |location| input[location + 2..].trim())
}

/// Check if line is blank.
///
/// Returns true if line is just whitespace.
pub(crate) fn is_blank(line: &str) -> bool {
    line.trim().is_empty()
}

/// Check if line is comment.
///
/// Returns true if the first non-blank text on the line is `//`.
pub(crate) fn is_comment(line: &str) -> bool {
    line.trim_start().starts_with("//")
}

/// Check if line is start.
///
/// Returns true if the first word is `_start`, in any case.
pub(crate) fn is_start(line: &str) -> bool {
    line.split_whitespace().next().is_some_and(|word| word.eq_ignore_ascii_case("_start"))
}

/// Extracts data name from string.
///
/// Returns the first word when it starts with `#`.
pub(crate) fn data_name_from_string(line: &str) -> Option<&str> {
    line.split_whitespace().next().filter(|word| word.starts_with('#'))
}

/// Extracts label from string.
///
/// Returns the first word when it ends with `:`.
pub(crate) fn label_name_from_string(line: &str) -> Option<&str> {
    line.split_whitespace().next().filter(|word| word.ends_with(':'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    // Test that comment is stripped
    fn test_strip_comments() {
        assert_eq!(strip_comments("Hello, world! //This is a comment"), "Hello, world!");
        assert_eq!(strip_comments("Hello, world! //"), "Hello, world!");
        assert_eq!(strip_comments(""), "");
    }

    #[test]
    // Test that comment is returned
    fn test_return_comments() {
        assert_eq!(return_comments("Hello, world! //This is a comment"), "This is a comment");
        assert_eq!(return_comments("Hello, world! //"), "");
        assert_eq!(return_comments("Hello, world!"), "");
    }

    #[test]
    // Test true is returned for comment
    fn test_is_comment1() {
        assert!(is_comment("//This is a comment"));
        assert!(is_comment("      //This is a comment"));
    }

    #[test]
    // Test false is returned for non-comment
    fn test_is_comment2() {
        assert!(!is_comment("Hello //This is a comment"));
        assert!(!is_comment(" "));
    }

    #[test]
    // The start marker is the first word `_start`, in any case.
    fn test_is_start() {
        assert!(is_start("_start"));
        assert!(is_start("  _START // entry"));
        assert!(!is_start("start"));
        assert!(!is_start("NOP _start"));
        assert!(!is_start(""));
    }

    #[test]
    // Test for blank line returns true
    fn test_is_blank1() {
        assert!(is_blank(" "));
        assert!(is_blank(""));
    }

    #[test]
    // Test for non blank line returns false
    fn test_is_blank2() {
        assert!(!is_blank("1234"));
        assert!(!is_blank("    1234"));
    }

    #[test]
    // Test for correct label name
    fn test_label_name_from_string3() {
        let input = String::from("LOOP:");
        let output = label_name_from_string(&input);
        assert_eq!(output, Some("LOOP:"));
    }

    #[test]
    // Test for invalid label name
    fn test_label_name_from_string4() {
        let input = String::from("LOOP");
        let output = label_name_from_string(&input);
        assert_eq!(output, None);
    }

    #[test]
    // Test for correct data name
    fn test_data_name_from_string1() {
        let input = String::from("#TEST");
        let output = data_name_from_string(&input);
        assert_eq!(output, Some("#TEST"));
    }

    #[test]
    // Test for invalid data name
    fn test_data_name_from_string2() {
        let input = String::from("TEST");
        let output = data_name_from_string(&input);
        assert_eq!(output, None);
    }

    #[test]
    // Check that labels are correctly extracted from strings test for label
    fn test_label_name_from_string1() {
        assert_eq!(label_name_from_string("label:"), Some("label:"));
        assert_eq!(label_name_from_string("label: "), Some("label:"));
        assert_eq!(label_name_from_string("label:"), Some("label:"));
        assert_eq!(label_name_from_string("     label:"), Some("label:"));
        assert_eq!(label_name_from_string("     label: dummy words"), Some("label:"));
    }

    #[test]
    // Check that none is returned if not a label
    fn test_label_name_from_string2() {
        assert_eq!(label_name_from_string("label :"), None);
        assert_eq!(label_name_from_string("label : "), None);
        assert_eq!(label_name_from_string("label"), None);
        assert_eq!(label_name_from_string("label "), None);
        assert_eq!(label_name_from_string(" label"), None);
        assert_eq!(label_name_from_string(" label "), None);
        assert_eq!(label_name_from_string("lab:el"), None);
        assert_eq!(label_name_from_string("  xxxxxxx   label:"), None);
    }
}
