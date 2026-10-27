//! rigor-rs#412 (ADR-0045): `rigor check` never dies of a stack overflow.
//!
//! These run the real binary, because the fix is the binary's thread setup:
//! a 256 MiB command thread and 256 MiB rayon workers, plus the depth bound
//! checked right after the parse.
//!
//! - One file runs on the command thread; adding a second file moves the deep
//!   one onto a rayon worker. Both paths are checked.
//! - The depths are picked for a debug binary (`cargo test`). Before #412:
//!   - 3,000 nested arrays overflowed both paths in debug, and the worker
//!     path in release;
//!   - the 30,000-link chain overflowed both paths in both builds. It is past
//!     the bound now, so it is refused before any walker runs.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

/// A self-cleaning unique temp directory (no external crate dependency).
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("rigor-{tag}-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create temp dir");
        TempDir(path)
    }

    fn write(&self, name: &str, text: &str) {
        fs::write(self.0.join(name), text).expect("write source");
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn rigor_check(cwd: &Path, files: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_rigor"))
        .current_dir(cwd)
        .env("RIGOR_NO_RUBY", "1")
        .env_remove("RIGOR_RUBY")
        .env_remove("RUST_MIN_STACK")
        .args(["check", "--format", "json"])
        .args(files)
        .output()
        .expect("run rigor")
}

/// The JSON rows of a run that must have exited normally: no signal, no
/// overflow message, exit 0 or 1, and a JSON array on stdout.
fn rows(out: &Output) -> Vec<serde_json::Value> {
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("overflowed its stack"), "stack overflow: {stderr}");
    assert!(
        matches!(out.status.code(), Some(0 | 1)),
        "abnormal exit {:?}: {stderr}",
        out.status
    );
    serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("stdout is not a JSON array ({e}): {stderr}"))
}

const TYPO: &str = "s = \"Hello\"\ns.lenght\n";

#[test]
fn deep_nesting_survives_on_the_command_thread_and_on_a_worker() {
    let dir = TempDir::new("deep-array");
    dir.write("a.rb", &format!("{}{}\n", "[".repeat(3000), "]".repeat(3000)));
    dir.write("b.rb", TYPO);

    assert!(rows(&rigor_check(dir.path(), &["a.rb"])).is_empty());
    let both = rows(&rigor_check(dir.path(), &["a.rb", "b.rb"]));
    assert_eq!(both.len(), 1, "{both:?}");
    assert_eq!(both[0]["path"], "b.rb");
    assert_eq!(both[0]["rule"], "call.undefined-method");
}

#[test]
fn a_chain_past_the_depth_bound_is_skipped_like_a_panicked_file() {
    let dir = TempDir::new("deep-chain");
    dir.write("a.rb", &format!("x{}\n", ".a".repeat(30_000)));
    dir.write("b.rb", TYPO);

    for files in [&["a.rb"][..], &["a.rb", "b.rb"][..]] {
        let out = rigor_check(dir.path(), files);
        let rows = rows(&out);
        let skipped: Vec<_> = rows.iter().filter(|r| r["path"] == "a.rb").collect();
        assert_eq!(skipped.len(), 1, "{rows:?}");
        assert_eq!(skipped[0]["rule"], "internal-error");
        assert_eq!(skipped[0]["severity"], "info");
        let message = skipped[0]["message"].as_str().unwrap();
        assert!(
            message.contains("depth 30003 exceeds the analysis limit of 25000"),
            "{message}"
        );
        // The panicked-file path, verbatim: the stderr line and a failing run.
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains("rigor check: internal panic on a.rb: "), "{stderr}");
        assert_eq!(out.status.code(), Some(1));
        // The rest of the run is analysed as usual.
        assert_eq!(rows.len(), files.len(), "{rows:?}");
    }
}
