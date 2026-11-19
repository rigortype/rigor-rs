//! The pool-skip whitelist is fail-closed (rigor-rs#678): commands that
//! only print static text run with a poisoned global pool, so a skipped
//! path that reached rayon would panic (exit 101) instead of silently
//! building a default pool on small stacks. `help <cmd>` re-dispatches
//! `<cmd> --help`, so this runs it for every catalogue command: exit 101
//! anywhere means the command touched rayon before the pool was built.
//! Expected codes are the command's own help surface, asserted so a
//! regression reads as a diff here, not as a silent lazy pool.

use std::process::Command;

/// `CATALOGUE_NAMES` lives in the binary's `main.rs`, which an integration
/// test cannot import — read it out of the source, so a command added to
/// the catalogue is covered without touching this table.
fn catalogue_names() -> Vec<String> {
    let src = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/main.rs"),
    )
    .expect("read src/main.rs");
    let start = src
        .find("const CATALOGUE_NAMES")
        .expect("CATALOGUE_NAMES in main.rs");
    let block = &src[start..src[start..].find("];").map(|e| start + e).unwrap()];
    block
        .split('"')
        .skip(1)
        .step_by(2)
        .map(str::to_string)
        .collect()
}

fn rigor(args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_rigor"))
        .args(args)
        .output()
        .expect("run rigor");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn every_catalogue_help_topic_runs_without_the_pool() {
    let names = catalogue_names();
    assert!(names.len() > 20, "CATALOGUE_NAMES parse found {names:?}");
    for name in &names {
        let (code, _stdout, stderr) = rigor(&["help", name]);
        assert_ne!(code, 101, "rigor help {name} reached rayon: {stderr}");
        // `lens` is not yet a dispatch arm (unknown command → 2) and
        // `playground` reports the missing gem (64, as the reference does);
        // every other topic's --help prints and exits 0.
        let expected = match name.as_str() {
            "lens" => 2,
            "playground" => 64,
            _ => 0,
        };
        assert_eq!(
            code, expected,
            "rigor help {name}: {stderr}"
        );
    }
}

#[test]
fn the_static_text_commands_run_without_the_pool() {
    for args in [
        &[][..],
        &["-h"][..],
        &["--help"][..],
        &["help"][..],
        &["version"][..],
        &["--version"][..],
        &["-v"][..],
        &["-V"][..],
        &["upgrade"][..],
    ] {
        let (code, _stdout, stderr) = rigor(args);
        assert_eq!(code, 0, "rigor {args:?}: {stderr}");
    }
}

#[test]
fn an_analysis_command_still_builds_the_pool() {
    // Control: the poisoned-pool branch must not leak into a command that
    // needs the pool — a clean check exits 0 and reports no findings.
    let dir = std::env::temp_dir().join(format!("rigor-p678-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("a.rb");
    std::fs::write(&file, "x = 1\n").unwrap();
    let (code, _stdout, stderr) = rigor(&["check", "--no-cache", file.to_str().unwrap()]);
    std::fs::remove_dir_all(&dir).ok();
    assert_eq!(code, 0, "check: {stderr}");
}
