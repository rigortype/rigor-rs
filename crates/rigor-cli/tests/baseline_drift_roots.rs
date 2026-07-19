//! End-to-end regression coverage for the `baseline drift`/`prune` positional-root
//! extension and the silently-empty-audit guard.
//!
//! The defect (branch `baseline-drift-roots`): `drift`/`prune` dropped positional
//! path arguments and analyzed ONLY the config `paths:` directive. On a project
//! with no `.rigor.yml` (empty config `paths:`), `generate .` wrote a full
//! baseline but `drift .` then analyzed nothing and reported every bucket as
//! "Cleared" — dangerously misleading, and a follow-up `prune` would empty the
//! baseline.
//!
//! The fix mirrors `generate`'s existing rigor-rs extension: positionals-if-given,
//! else config `paths:`. When the RESOLVED analysis set is empty (no positional
//! AND no config `paths:`) against a non-empty baseline, drift/prune now refuse
//! (exit 64) rather than report all-cleared.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A self-cleaning unique temp directory (no external crate dependency).
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "rigor-{tag}-{}-{}-{n}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&path).expect("create temp dir");
        TempDir(path)
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

/// Run the `rigor` binary under test with `cwd`, `RIGOR_NO_RUBY=1` (hermetic:
/// the sound Ruby-free subset still fires the `String#lenght` typo), returning
/// `(stdout, stderr, exit_code)`.
fn run_rigor(cwd: &Path, args: &[&str]) -> (String, String, i32) {
    let out = Command::new(env!("CARGO_BIN_EXE_rigor"))
        .current_dir(cwd)
        .env("RIGOR_NO_RUBY", "1")
        // Neutralize any ambient config that could inject a `paths:` directive.
        .env_remove("RIGOR_RUBY")
        .args(args)
        .output()
        .expect("failed to spawn rigor binary");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

/// A fresh temp project dir with a single `.rb` file carrying one known
/// diagnostic (`String#lenght` typo) and NO `.rigor.yml` (empty config `paths:`).
fn fixture_project() -> TempDir {
    let dir = TempDir::new("baseline-drift");
    fs::write(dir.path().join("sample.rb"), b"s = \"Hello\"\ns.lenght\n").expect("write fixture");
    dir
}

#[test]
fn generate_then_drift_with_positional_root_reports_zero_drift() {
    let dir = fixture_project();
    let bl = dir.path().join("bl.yml");
    let bl_str = bl.to_str().unwrap();

    // generate over the positional root `.` writes a non-empty baseline.
    let (_o, gen_err, gen_code) =
        run_rigor(dir.path(), &["baseline", "generate", ".", "--output", bl_str, "--force"]);
    assert_eq!(gen_code, 0, "generate should succeed; stderr: {gen_err}");
    assert!(bl.exists(), "baseline file should be written");
    let bl_body = fs::read_to_string(&bl).unwrap();
    assert!(
        bl_body.contains("sample.rb"),
        "baseline should record the sample.rb diagnostic; got:\n{bl_body}"
    );

    // drift over the SAME positional root must see the same diagnostic → no drift.
    let (drift_out, drift_err, drift_code) =
        run_rigor(dir.path(), &["baseline", "drift", ".", "--baseline", bl_str]);
    assert_eq!(drift_code, 0, "drift should succeed; stderr: {drift_err}");
    assert!(
        drift_out.contains("No drift detected."),
        "drift with a positional root must report no drift, NOT all-cleared; got stdout:\n{drift_out}\nstderr:\n{drift_err}"
    );
    assert!(
        !drift_out.contains("Cleared"),
        "drift must not report any Cleared bucket; got:\n{drift_out}"
    );
}

#[test]
fn drift_without_roots_or_config_paths_against_nonempty_baseline_errors() {
    let dir = fixture_project();
    let bl = dir.path().join("bl.yml");
    let bl_str = bl.to_str().unwrap();

    // Seed a non-empty baseline (via a positional root).
    let (_o, _e, gen_code) =
        run_rigor(dir.path(), &["baseline", "generate", ".", "--output", bl_str, "--force"]);
    assert_eq!(gen_code, 0);

    // drift with NO positional AND no config `paths:` must refuse (exit 64),
    // not silently report all-cleared.
    let (drift_out, drift_err, drift_code) =
        run_rigor(dir.path(), &["baseline", "drift", "--baseline", bl_str]);
    assert_eq!(
        drift_code, 64,
        "drift with an empty analysis set + non-empty baseline must exit 64; stdout:\n{drift_out}\nstderr:\n{drift_err}"
    );
    assert!(
        drift_err.contains("nothing to analyze"),
        "expected a 'nothing to analyze' usage error on stderr; got:\n{drift_err}"
    );
    assert!(
        !drift_out.contains("Cleared"),
        "must not have printed a Cleared report; got stdout:\n{drift_out}"
    );
}

#[test]
fn drift_with_declared_config_paths_and_no_positional_does_not_error() {
    // A declared `paths:` IS an analysis scope — the guard must not fire, and
    // drift audits that scope (here `.`) exactly as with a positional root.
    let dir = fixture_project();
    fs::write(dir.path().join(".rigor.yml"), b"paths:\n  - .\n").expect("write config");
    let bl = dir.path().join("bl.yml");
    let bl_str = bl.to_str().unwrap();

    let (_o, _e, gen_code) =
        run_rigor(dir.path(), &["baseline", "generate", "--output", bl_str, "--force"]);
    assert_eq!(gen_code, 0);

    let (drift_out, drift_err, drift_code) =
        run_rigor(dir.path(), &["baseline", "drift", "--baseline", bl_str]);
    assert_eq!(drift_code, 0, "declared paths must not trip the guard; stderr:\n{drift_err}");
    assert!(
        drift_out.contains("No drift detected."),
        "drift over the declared scope must report no drift; got:\n{drift_out}"
    );
}

#[test]
fn prune_without_roots_or_config_paths_against_nonempty_baseline_errors() {
    let dir = fixture_project();
    let bl = dir.path().join("bl.yml");
    let bl_str = bl.to_str().unwrap();

    let (_o, _e, gen_code) =
        run_rigor(dir.path(), &["baseline", "generate", ".", "--output", bl_str, "--force"]);
    assert_eq!(gen_code, 0);
    let before = fs::read_to_string(&bl).unwrap();

    // prune with an empty analysis set must refuse rather than empty the baseline.
    let (prune_out, prune_err, prune_code) =
        run_rigor(dir.path(), &["baseline", "prune", "--baseline", bl_str]);
    assert_eq!(
        prune_code, 64,
        "prune with an empty analysis set + non-empty baseline must exit 64; stdout:\n{prune_out}\nstderr:\n{prune_err}"
    );
    assert!(
        prune_err.contains("nothing to analyze"),
        "expected a 'nothing to analyze' usage error on stderr; got:\n{prune_err}"
    );
    // The baseline must be untouched (not emptied).
    let after = fs::read_to_string(&bl).unwrap();
    assert_eq!(before, after, "prune must not have modified the baseline");
}
