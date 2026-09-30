//! Builds the small C decoder shim and links it against the system FFmpeg (libavcodec + libavutil).
//!
//! FFmpeg is located via, in order:
//!   1. `FFMPEG_DIR` (expects `include/` and `lib/`; the usual way on Windows)
//!   2. `pkg-config` (Linux)
//!   3. Homebrew's default prefix (macOS)

use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=csrc/vd_decoder.c");
    println!("cargo:rerun-if-changed=csrc/vd_decoder.h");
    println!("cargo:rerun-if-env-changed=FFMPEG_DIR");

    let mut build = cc::Build::new();
    build.file("csrc/vd_decoder.c").warnings(true);

    let libs = ["avcodec", "avutil"];
    if let Ok(dir) = env::var("FFMPEG_DIR") {
        let dir = PathBuf::from(dir);
        build.include(dir.join("include"));
        println!(
            "cargo:rustc-link-search=native={}",
            dir.join("lib").display()
        );
        for l in libs {
            println!("cargo:rustc-link-lib={l}");
        }
    } else if let Some((includes, search, names)) = pkg_config(&["libavcodec", "libavutil"]) {
        for i in includes {
            build.include(i);
        }
        for s in search {
            println!("cargo:rustc-link-search=native={s}");
        }
        for n in names {
            println!("cargo:rustc-link-lib={n}");
        }
    } else if cfg!(target_os = "macos")
        && PathBuf::from("/opt/homebrew/opt/ffmpeg/include").exists()
    {
        build.include("/opt/homebrew/opt/ffmpeg/include");
        println!("cargo:rustc-link-search=native=/opt/homebrew/opt/ffmpeg/lib");
        for l in libs {
            println!("cargo:rustc-link-lib={l}");
        }
    } else {
        panic!(
            "FFmpeg not found: set FFMPEG_DIR (with include/ and lib/) or install FFmpeg development packages"
        );
    }

    build.compile("vd_decoder");
}

/// Returns (include dirs, link search dirs, lib names) from `pkg-config`, if available.
fn pkg_config(pkgs: &[&str]) -> Option<(Vec<String>, Vec<String>, Vec<String>)> {
    let out = Command::new("pkg-config")
        .arg("--cflags")
        .arg("--libs")
        .args(pkgs)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let (mut inc, mut search, mut names) = (vec![], vec![], vec![]);
    for tok in String::from_utf8(out.stdout).ok()?.split_whitespace() {
        if let Some(v) = tok.strip_prefix("-I") {
            inc.push(v.to_string());
        } else if let Some(v) = tok.strip_prefix("-L") {
            search.push(v.to_string());
        } else if let Some(v) = tok.strip_prefix("-l") {
            names.push(v.to_string());
        }
    }
    Some((inc, search, names))
}
