//! The files the tool writes: the `.code` listing, the program image
//! formats and the opcode documentation, and how output files are named.

pub(crate) mod docs;
pub(crate) mod image;
pub(crate) mod listing;

use std::ffi::OsStr;
use std::fs;
use std::io::Error;
use std::path::Path;

/// Return the stem of given filename.
///
/// Looks for first dot in the string, and returns the slice before the dot.
pub(crate) fn filename_stem(full_name: &str) -> String {
    let path = Path::new(full_name);
    let stem = path.file_stem();
    let parent = path
        .parent()
        .unwrap_or_else(|| Path::new(""))
        .join(stem.unwrap_or_else(|| OsStr::new("")));

    parent.to_str().unwrap_or_default().to_owned()
}

/// Name an output file with the given extension (without the dot).
///
/// An explicit name (from `-o` / `-b`) is used as given, with the extension
/// appended unless it already ends with it (so `-b out.kbt` stays `out.kbt`).
/// Without one, the extension replaces the input file's.
pub(crate) fn output_file_name(explicit: Option<&str>, input_file_name: &str, extension: &str) -> String {
    let suffix = format!(".{extension}");
    match explicit {
        Some(name) if name.to_ascii_lowercase().ends_with(&suffix.to_ascii_lowercase()) => name.to_owned(),
        Some(name) => format!("{name}{suffix}"),
        None => format!("{}{suffix}", filename_stem(input_file_name)),
    }
}

/// Output the bitcode to given file.
///
/// Based on the bitcode string outputs to file.
pub(crate) fn write_binary_output_file(filename: &impl AsRef<Path>, output_string: &str) -> Result<(), Error> {
    fs::write(filename, output_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::MAIN_SEPARATOR_STR;
    use tempfile::TempDir;

    #[test]
    // Check correct filename stem is returned
    fn test_filename_stem() {
        assert_eq!(filename_stem("file.type"), "file");
        assert_eq!(filename_stem("file"), "file");
        assert_eq!(
            filename_stem(&format!("{MAIN_SEPARATOR_STR}my_path{MAIN_SEPARATOR_STR}file.kla")),
            format!("{MAIN_SEPARATOR_STR}my_path{MAIN_SEPARATOR_STR}file")
        );
        assert_eq!(
            filename_stem(&format!("relative_path{MAIN_SEPARATOR_STR}file.kla")),
            format!("relative_path{MAIN_SEPARATOR_STR}file")
        );
    }

    #[test]
    fn output_file_name_appends_extension_once() {
        assert_eq!(output_file_name(Some("out"), "prog.kla", "kbt"), "out.kbt");
        assert_eq!(output_file_name(Some("out.kbt"), "prog.kla", "kbt"), "out.kbt");
        assert_eq!(output_file_name(Some("OUT.KBT"), "prog.kla", "kbt"), "OUT.KBT");
        assert_eq!(output_file_name(Some("out.code"), "prog.kla", "kbt"), "out.code.kbt");
        assert_eq!(output_file_name(Some("prog.kla"), "prog.kla", "code"), "prog.kla.code");
    }

    #[test]
    fn output_file_name_defaults_to_input_stem_and_keeps_spaces() {
        assert_eq!(output_file_name(None, "prog.kla", "kbt"), "prog.kbt");
        let input = format!("my dir{MAIN_SEPARATOR_STR}my prog.kla");
        assert_eq!(output_file_name(None, &input, "code"), format!("my dir{MAIN_SEPARATOR_STR}my prog.code"));
    }

    #[test]
    fn test_write_binary_output_file() {
        let tmp_dir = TempDir::new().unwrap();

        let file_path1 = tmp_dir.path().join("test1.klb");
        let binding = file_path1;
        let file_name1: &str = binding.to_str().unwrap();

        let result_write = write_binary_output_file(&file_name1, "12345678");
        result_write.unwrap();

        let bytes = fs::read(file_name1).unwrap();
        assert_eq!(bytes, b"12345678");
    }
}
