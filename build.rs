//! Build script: with the `board` feature, tell the linker where libftdi1 is.
//!
//! `src/jtag/ftdi.rs` links `ftdi1` by name (`#[link(name = "ftdi1")]`),
//! which finds it in the default library paths (Debian/Ubuntu's
//! `libftdi1-dev`).  Elsewhere, e.g. Homebrew on Apple silicon (under
//! `/opt/homebrew`), the library directories `pkg-config` reports for
//! libftdi1 are added to the search path.  Without `pkg-config` or a
//! `libftdi1.pc`, the build goes on with the plain link name and a warning.

use std::env;
use std::process::Command;

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    if env::var_os("CARGO_FEATURE_BOARD").is_none() {
        return;
    }
    println!("cargo::rerun-if-env-changed=PKG_CONFIG");
    println!("cargo::rerun-if-env-changed=PKG_CONFIG_PATH");
    println!("cargo::rerun-if-env-changed=PKG_CONFIG_LIBDIR");
    match libftdi_dirs() {
        Ok(dirs) => {
            for dir in dirs {
                println!("cargo::rustc-link-search=native={dir}");
            }
        }
        Err(why) => println!("cargo::warning=pkg-config did not find libftdi1 ({why}); linking ftdi1 from the default library paths"),
    }
}

/// The library directories `pkg-config` (or `$PKG_CONFIG`) reports for libftdi1.
fn libftdi_dirs() -> Result<Vec<String>, String> {
    let pkg_config = env::var("PKG_CONFIG").unwrap_or_else(|_| "pkg-config".to_owned());
    let out = Command::new(&pkg_config)
        .args(["--libs-only-L", "libftdi1"])
        .output()
        .map_err(|e| format!("cannot run {pkg_config}: {e}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let why = stderr.lines().last().unwrap_or("no libftdi1.pc").trim();
        return Err(why.to_owned());
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .filter_map(|flag| flag.strip_prefix("-L"))
        .map(str::to_owned)
        .collect())
}
