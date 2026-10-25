//! Pure text predicates for classifying one line of assembler source.
//!
//! These look only at the text: whether a line is blank, a comment, a label,
//! a data definition or the start marker, and how to split off a trailing `//`
//! comment.  Classification that needs the opcode table (`line_type`) stays
//! with the table.  This module has no intra-crate dependencies.

/// Strip trailing comments.
///
/// Removes comments and starting and training whitespace.
pub fn strip_comments(input: &str) -> String {
    input.find("//").map_or_else(
        || input.trim().to_owned(),
        |location| input.get(0..location).unwrap_or("").trim().to_owned(),
    )
}

/// Returns trailing comments.
///
/// Removes comments and starting and training whitespace.
pub fn return_comments(input: &str) -> String {
    input
        .find("//")
        .map_or_else(String::default, |location| input.get(location + 2..).unwrap_or("").trim().to_owned())
}

/// Check if line is blank.
///
/// Returns true if line if just whitespace.
pub fn is_blank(line: &str) -> bool {
    let words = line.split_whitespace();

    for word in words {
        if !word.is_empty() {
            return false;
        }
    }
    true
}

/// Check if line is comment.
///
/// Returns true if line if just comment.
pub fn is_comment(line: &str) -> bool {
    let word = line.trim();
    if word.len() < 2 {
        return false;
    }

    let bytes = word.as_bytes();
    let mut found_first = false;

    for (i, &item) in bytes.iter().enumerate() {
        if item == b'/' && i == 0 {
            found_first = true;
        }
        if item == b'/' && i == 1 && found_first {
            return true;
        }
    }
    false
}

/// Check if line is start.
///
/// Returns true if line is start.
pub fn is_start(line: &str) -> bool {
    let words = line.split_whitespace();
    for (i, word) in words.enumerate() {
        if i == 0 && word.to_uppercase() == "_START" {
            return true;
        }
    }
    false
}

/// Extracts data name from string.
///
/// Checks if start of first word is hash if so return data name as option string.
pub fn data_name_from_string(line: &str) -> Option<String> {
    let mut words = line.split_whitespace();
    let first_word = words.next().unwrap_or("");
    if first_word.starts_with('#') {
        return Some(first_word.to_owned());
    }
    None
}

/// Extracts label from string.
///
/// Checks if end of first word is colon if so return label as option string.
pub fn label_name_from_string(line: &str) -> Option<String> {
    let mut words = line.split_whitespace();
    let first_word = words.next().unwrap_or("");
    if first_word.ends_with(':') {
        return Some(first_word.to_owned());
    }
    None
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
        assert_eq!(output, Some("LOOP:".to_owned()));
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
        assert_eq!(output, Some("#TEST".to_owned()));
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
        assert_eq!(label_name_from_string("label:"), Some("label:".to_owned()));
        assert_eq!(label_name_from_string("label: "), Some("label:".to_owned()));
        assert_eq!(label_name_from_string("label:"), Some("label:".to_owned()));
        assert_eq!(label_name_from_string("     label:"), Some("label:".to_owned()));
        assert_eq!(label_name_from_string("     label: dummy words"), Some("label:".to_owned()));
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
