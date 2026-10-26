//! Loading an ELF or flat binary as a board program.
//!
//! An ELF's LOAD segments are flattened into one buffer that the board loads
//! straight after the heap header, at [`CODE_BASE`]; any other file is taken
//! as a flat binary, verbatim.  [`load_flat`] is the one entry point every
//! binary-input mode uses (`--net-load`, `--mem-out`, elf2serial,
//! `--emulate`, `--gdbserver`).

use crate::diag::Diagnostics;
use crate::error::Error;
use crate::helper::human_bytes;
use crate::layout::CODE_BASE;
use crate::output::image::Image;
use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;

/// Magic bytes at the start of every ELF file (`0x7F` `E` `L` `F`).
pub(crate) const ELF_MAGIC: &[u8] = b"\x7fELF";

/// A binary input file, and the entry point to use for it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct BinaryInput<'a> {
    /// The ELF or flat binary file.
    pub(crate) path: &'a str,
    /// `--entry`: the entry point to use instead of the ELF's own (or a flat
    /// binary's default of `0x20`).
    pub(crate) entry: Option<u32>,
}

/// An input file loaded as a board program.
#[derive(Debug)]
pub(crate) struct Loaded {
    /// The program and its board entry point.  The bytes are not padded.
    pub(crate) image: Image,
    /// The ELF layout, or `None` for a flat binary.
    pub(crate) elf: Option<ElfLayout>,
}

impl Loaded {
    /// The log line describing what was loaded: the ELF's size, base and
    /// entry (and, with `board_entry`, the entry as a board address), or that
    /// it is a flat binary.
    pub(crate) fn detected_message(&self, board_entry: bool) -> String {
        let Some(elf) = &self.elf else {
            return "Detected flat binary (no ELF header)".to_owned();
        };
        let mut text = format!(
            "Detected ELF file: {}, ELF base 0x{:08X}, ELF entry 0x{:08X}",
            human_bytes(self.image.bytes.len()),
            elf.base,
            elf.entry
        );
        if board_entry {
            let _ = write!(text, ", board entry 0x{:08X}", self.image.entry);
        }
        text
    }
}

/// Read `input.path` and flatten it into a board program ([`flatten_input`]).
///
/// `input.entry` (`--entry`) wins over the ELF's entry point and the flat
/// binary default of `0x20`.  A file that cannot be read, or an ELF with no
/// loadable segments, is reported and returned as the command's failure.
#[cfg(not(tarpaulin_include))]
pub(crate) fn load_flat(input: BinaryInput<'_>, msg_list: &mut Diagnostics) -> Result<Loaded, Error> {
    let path = input.path;
    let file_data = fs::read(path).map_err(|source| {
        msg_list.error(format!("Cannot read binary file {path}: {source}"));
        Error::Read {
            path: PathBuf::from(path),
            source,
        }
    })?;
    let Some((bytes, entry, elf)) = flatten_input(file_data, input.entry) else {
        msg_list.error(format!("Failed to extract LOAD segments from ELF file {path}"));
        return Err(Error::Elf(PathBuf::from(path)));
    };
    Ok(Loaded {
        image: Image::new(bytes, entry),
        elf,
    })
}

/// Parse an ELF file and extract its LOAD segments as a contiguous flat buffer.
///
/// Returns `(flat_bytes, base_address, entry_address)` where `flat_bytes` covers
/// the range `[base_address, base_address + flat_bytes.len())`.  Gaps between
/// non-contiguous LOAD segments are zero-filled.  Returns `None` if the file
/// cannot be parsed or contains no LOAD segments with data.
fn parse_elf_to_flat(data: &[u8]) -> Option<(Vec<u8>, u64, u64)> {
    use object::{Object as _, ObjectSegment as _, ObjectSymbol as _};
    /// Max gap between contiguous LOAD segments before the rest is dropped (see below).
    const MAX_SEGMENT_GAP: u64 = 0x40_0000; // 4 MB
    let file = object::File::parse(data).ok()?;

    // Collect all LOAD segments that actually have bytes in the file.
    let mut segments: Vec<(u64, Vec<u8>)> = file
        .segments()
        .filter_map(|seg| {
            let addr = seg.address();
            let bytes = seg.data().ok()?.to_vec();
            if bytes.is_empty() { None } else { Some((addr, bytes)) }
        })
        .collect();
    segments.sort_by_key(|(addr, _)| *addr);

    if segments.is_empty() {
        return None;
    }

    // Only keep the first contiguous cluster of segments.  Embedded ELFs often
    // place code/data at a low VMA and a completely separate region (e.g. XIP
    // flash, ITCM, peripheral windows) at a very high VMA.  Zero-filling the
    // gap between them would produce a multi-hundred-MB flat buffer that stalls
    // encoding and disassembly.  Segments separated by more than MAX_SEGMENT_GAP
    // bytes from the previous one are treated as a different physical memory
    // region and dropped from this cluster.
    let cluster: Vec<(u64, Vec<u8>)> = segments
        .into_iter()
        .scan(None::<u64>, |prev_end, (addr, bytes)| {
            let gap = prev_end.map_or(0, |end| addr.saturating_sub(end));
            *prev_end = Some(addr + bytes.len() as u64);
            if gap <= MAX_SEGMENT_GAP { Some(Some((addr, bytes))) } else { Some(None) }
        })
        .take_while(Option::is_some)
        .flatten()
        .collect();

    if cluster.is_empty() {
        return None;
    }

    let base = cluster[0].0;
    let end = cluster.iter().map(|(addr, bytes)| addr + bytes.len() as u64).max()?;
    let mut flat = vec![0_u8; (end - base) as usize];
    for (addr, bytes) in &cluster {
        let offset = (addr - base) as usize;
        flat[offset..offset + bytes.len()].copy_from_slice(bytes);
    }

    // Determine entry point:
    // 1. ELF e_entry header field (set by linker ENTRY() directive)
    // 2. Fall back to _start symbol address
    // 3. Final fallback: base address of first LOAD segment
    let entry = {
        let e = file.entry();
        if e != 0 {
            e
        } else {
            file.symbols().find(|sym| sym.name() == Ok("_start")).map_or(base, |sym| sym.address())
        }
    };

    Some((flat, base, entry))
}

/// ELF layout captured while flattening, kept for diagnostic logging.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ElfLayout {
    /// ELF virtual base address (VMA of the first LOAD segment).
    pub(crate) base: u64,
    /// ELF entry point (`e_entry` / `_start`) as an ELF virtual address.
    pub(crate) entry: u64,
}

/// Flatten a loaded input file into a board image payload plus entry address.
///
/// Shared core of every `--net-load` / `--mem-out` / `elf2serial` / `--emulate`
/// path.  If `file_data` is an ELF (detected via [`ELF_MAGIC`]) its LOAD segments
/// are extracted ([`parse_elf_to_flat`]) and the entry point is translated from
/// ELF VMA to a board byte address: `(elf_entry - elf_base) + HEAP_HEADER_WORDS*8`
/// (the board loads the image immediately after the heap header).  Otherwise the
/// bytes are used verbatim with a default board entry of `0x20`.  `entry_override`,
/// when set, wins in both cases.
///
/// Returns `None` only when ELF parsing fails.  The returned `Option<ElfLayout>`
/// is `Some` for ELF inputs (for the caller's log line) and `None` for flat
/// binaries.  The returned bytes are NOT padded — callers that need 4-byte
/// alignment pad afterwards.
fn flatten_input(file_data: Vec<u8>, entry_override: Option<u32>) -> Option<(Vec<u8>, u32, Option<ElfLayout>)> {
    if file_data.starts_with(ELF_MAGIC) {
        let (flat, elf_base, elf_entry) = parse_elf_to_flat(&file_data)?;
        let board_entry = elf_entry.saturating_sub(elf_base) + u64::from(CODE_BASE);
        let entry = entry_override.unwrap_or(board_entry as u32);
        Some((
            flat,
            entry,
            Some(ElfLayout {
                base: elf_base,
                entry: elf_entry,
            }),
        ))
    } else {
        Some((file_data, entry_override.unwrap_or(0x20_u32), None))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Read a git-tracked ELF fixture from the test fixture corpus (path is absolute
    /// via `CARGO_MANIFEST_DIR`, so the test is independent of the working dir).
    fn fixture(name: &str) -> Vec<u8> {
        let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
        std::fs::read(&path).unwrap_or_else(|e| panic!("read fixture {path}: {e}"))
    }

    // ---- parse_elf_to_flat ---------------------------------------------------

    #[test]
    fn parse_elf_extracts_hello() {
        let (flat, base, entry) = parse_elf_to_flat(&fixture("hello.elf")).expect("hello.elf should parse");
        assert_eq!(base, 0x20, "first LOAD segment VMA");
        assert_eq!(entry, 0x20, "entry point (e_entry)");
        assert_eq!(flat.len(), 3980, "flattened LOAD-segment image length");
        // The entry point must land inside the loaded image.
        assert!(entry >= base && (entry - base) < flat.len() as u64, "entry within loaded range");
    }

    #[test]
    fn parse_elf_extracts_test_64bit() {
        let (flat, base, entry) = parse_elf_to_flat(&fixture("test_64bit.elf")).expect("should parse");
        assert_eq!((base, entry, flat.len()), (0x20, 0x20, 8507));
    }

    #[test]
    fn parse_elf_is_deterministic() {
        let data = fixture("hello.elf");
        assert_eq!(parse_elf_to_flat(&data), parse_elf_to_flat(&data));
    }

    #[test]
    fn parse_elf_rejects_non_elf() {
        assert!(parse_elf_to_flat(b"not an ELF file at all").is_none());
        assert!(parse_elf_to_flat(&[]).is_none());
    }

    #[test]
    fn parse_elf_rejects_truncated_elf() {
        // ELF magic but an otherwise-invalid/empty header → no usable LOAD segments.
        let mut bad = ELF_MAGIC.to_vec();
        bad.extend_from_slice(&[0_u8; 64]);
        assert!(parse_elf_to_flat(&bad).is_none());
    }

    // ---- flatten_input -------------------------------------------------------

    #[test]
    fn flatten_flat_binary_uses_default_entry() {
        let raw = vec![0xDE_u8, 0xAD, 0xBE, 0xEF];
        let (data, entry, elf) = flatten_input(raw.clone(), None).expect("flat path is infallible");
        assert_eq!(data, raw, "flat bytes pass through verbatim");
        assert_eq!(entry, 0x20, "default board entry for a flat binary");
        assert!(elf.is_none(), "no ELF metadata for a flat binary");
    }

    #[test]
    fn flatten_flat_binary_entry_override_wins() {
        let (_, entry, _) = flatten_input(vec![1, 2, 3], Some(0x1000)).expect("flat path");
        assert_eq!(entry, 0x1000);
    }

    #[test]
    fn flatten_elf_translates_entry_to_board_address() {
        let data = fixture("hello.elf");
        let (flat, base, elf_entry) = parse_elf_to_flat(&data).unwrap();
        let (fdata, entry, meta) = flatten_input(data, None).expect("ELF should flatten");

        assert_eq!(fdata, flat, "ELF path returns the parse_elf_to_flat buffer");
        let meta = meta.expect("ELF metadata present for ELF input");
        assert_eq!((meta.base, meta.entry), (base, elf_entry), "metadata mirrors parse output");

        // board entry = (elf_entry - elf_base) + heap-header bytes
        let expected = (elf_entry.saturating_sub(base) + u64::from(CODE_BASE)) as u32;
        assert_eq!(entry, expected, "VMA-to-board entry translation");
        assert_eq!(entry, 0x20, "hello.elf loads at the start of the post-header image");
    }

    #[test]
    fn flatten_elf_entry_override_wins() {
        let (_, entry, meta) = flatten_input(fixture("hello.elf"), Some(0xABC)).expect("ELF");
        assert_eq!(entry, 0xABC, "explicit --entry overrides the computed board entry");
        assert!(meta.is_some(), "override does not suppress ELF metadata");
    }

    #[test]
    fn flatten_rejects_bad_elf() {
        // Starts with ELF magic (so the flat fallback is skipped) but is not a
        // valid ELF → flatten_input propagates parse_elf_to_flat's None.
        let mut bad = ELF_MAGIC.to_vec();
        bad.extend_from_slice(&[0_u8; 64]);
        assert!(flatten_input(bad, None).is_none());
    }

    // ---- Loaded::detected_message -------------------------------------------

    #[test]
    fn detected_message_describes_an_elf() {
        let (bytes, entry, elf) = flatten_input(fixture("hello.elf"), None).unwrap();
        let loaded = Loaded {
            image: Image::new(bytes, entry),
            elf,
        };
        assert_eq!(
            loaded.detected_message(false),
            "Detected ELF file: 3.9 KiB, ELF base 0x00000020, ELF entry 0x00000020"
        );
        assert_eq!(
            loaded.detected_message(true),
            "Detected ELF file: 3.9 KiB, ELF base 0x00000020, ELF entry 0x00000020, board entry 0x00000020"
        );
    }

    #[test]
    fn detected_message_describes_a_flat_binary() {
        let loaded = Loaded {
            image: Image::new(vec![1, 2, 3], 0x20),
            elf: None,
        };
        assert_eq!(loaded.detected_message(true), "Detected flat binary (no ELF header)");
        assert_eq!(loaded.detected_message(false), "Detected flat binary (no ELF header)");
    }
}
