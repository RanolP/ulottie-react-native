//! Builds ThorVG (feature `thorvg`) from a pinned upstream source tarball.
//!
//! No meson, no submodule: the exact v1.1.1 release tarball is downloaded
//! once into the workspace target directory (sha256-pinned), and the SW-engine
//! source subset is compiled directly with the `cc` crate — the same
//! manifest-direct approach dotlottie-rs uses. The objects land inside this
//! crate's staticlib, so the iOS pod links one archive and needs no ThorVG
//! build of its own; `cc` cross-compiles for Apple targets via the active SDK.
//!
//! Compiled subset (mirrors the meson source lists for `-Dengines=cpu`
//! `-Dloaders=""` `-Dbindings=capi` `-Dthreads=false`):
//! src/common, src/renderer, src/renderer/cpu_engine, src/loaders/raw
//! (unconditional in upstream), src/bindings/capi.
//!
//! Override the source location with `ULOTTIE_THORVG_SRC=<dir>` (a directory
//! that contains `inc/thorvg.h`) to build offline or against a patched tree.

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

const THORVG_VERSION: &str = "1.1.1";
const THORVG_SHA256: &str = "59c12500b7c2fc426e89667b3e4f3fdc2ff05a75cc12001a22c5f58fb1cdf592";

fn main() {
    if env::var_os("CARGO_FEATURE_THORVG").is_none() {
        return;
    }
    let src = thorvg_src();
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());

    // ThorVG sources `#include "config.h"`; meson generates it, we do too.
    let config_dir = out.join("thorvg-config");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(
        config_dir.join("config.h"),
        format!(
            "#pragma once\n#define THORVG_VERSION_STRING \"{THORVG_VERSION}\"\n\
             #define THORVG_CPU_ENGINE_SUPPORT 1\n"
        ),
    )
    .unwrap();

    let dirs = [
        "src/common",
        "src/renderer",
        "src/renderer/cpu_engine",
        "src/loaders/raw",
        "src/bindings/capi",
    ];
    let mut build = cc::Build::new();
    build
        .cpp(true)
        .std("c++14")
        .include(&config_dir)
        .include(src.join("inc"))
        .define("TVG_STATIC", None)
        .flag_if_supported("-fno-exceptions")
        .flag_if_supported("-fno-rtti")
        .flag_if_supported("-fno-math-errno")
        .flag_if_supported("-Wno-unknown-pragmas")
        .warnings(false);
    for d in &dirs {
        build.include(src.join(d));
    }
    for d in &dirs {
        for entry in std::fs::read_dir(src.join(d)).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|e| e == "cpp") {
                build.file(path);
            }
        }
    }
    build.compile("thorvg");
    println!("cargo:rerun-if-env-changed=ULOTTIE_THORVG_SRC");
}

/// The ThorVG source tree: `$ULOTTIE_THORVG_SRC`, or the pinned tarball
/// fetched into `<target dir>/thorvg-src/thorvg-<version>` (shared across
/// host and cross target builds, downloaded once).
fn thorvg_src() -> PathBuf {
    if let Ok(dir) = env::var("ULOTTIE_THORVG_SRC") {
        let dir = PathBuf::from(dir);
        assert!(
            dir.join("inc/thorvg.h").is_file(),
            "ULOTTIE_THORVG_SRC={} does not contain inc/thorvg.h",
            dir.display()
        );
        return dir;
    }
    let cache = target_dir().join("thorvg-src");
    let src = cache.join(format!("thorvg-{THORVG_VERSION}"));
    if src.join("inc/thorvg.h").is_file() {
        return src;
    }
    std::fs::create_dir_all(&cache).unwrap();
    let tarball = cache.join(format!("thorvg-{THORVG_VERSION}.tar.gz"));
    let url = format!(
        "https://github.com/thorvg/thorvg/archive/refs/tags/v{THORVG_VERSION}.tar.gz"
    );
    run(
        Command::new("curl").args(["-sSfL", "-o"]).arg(&tarball).arg(&url),
        "download the ThorVG source tarball (set ULOTTIE_THORVG_SRC to build offline)",
    );
    // Pin the bytes, not just the tag: a moved tag or a MITM fails loudly.
    let sum = String::from_utf8(
        run(
            Command::new("shasum").args(["-a", "256"]).arg(&tarball),
            "hash the ThorVG tarball",
        )
        .stdout,
    )
    .unwrap();
    let got = sum.split_whitespace().next().unwrap_or("").to_string();
    assert_eq!(
        got, THORVG_SHA256,
        "ThorVG v{THORVG_VERSION} tarball hash mismatch ({})",
        tarball.display()
    );
    run(
        Command::new("tar").arg("xzf").arg(&tarball).arg("-C").arg(&cache),
        "extract the ThorVG tarball",
    );
    assert!(src.join("inc/thorvg.h").is_file(), "unexpected tarball layout");
    src
}

fn run(cmd: &mut Command, what: &str) -> std::process::Output {
    let out = cmd
        .output()
        .unwrap_or_else(|e| panic!("could not {what}: {:?}: {e}", cmd.get_program()));
    assert!(
        out.status.success(),
        "could not {what}: {:?} exited {}: {}",
        cmd,
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

/// The workspace target directory (where the source cache lives). OUT_DIR is
/// `<target>/<triple?>/<profile>/build/<crate>-<hash>/out`; walking up to the
/// component whose parent holds `CARGO_MANIFEST_DIR`'s workspace is fragile,
/// so derive it the simple way: CARGO_TARGET_DIR when set, else
/// `<workspace>/target` (the workspace root is this crate's parent).
fn target_dir() -> PathBuf {
    if let Ok(d) = env::var("CARGO_TARGET_DIR") {
        return PathBuf::from(d);
    }
    Path::new(&env::var("CARGO_MANIFEST_DIR").unwrap())
        .parent()
        .unwrap()
        .join("target")
}
