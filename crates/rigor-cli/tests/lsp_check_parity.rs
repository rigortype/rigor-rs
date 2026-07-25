//! LSP §12 **S4b** acceptance test 1 — the parity keystone.
//!
//! The `rigor lsp` server's diagnostics for a SAVED (non-dirty) buffer must equal
//! what `rigor check` reports for that file with PROJECT context. This is the test
//! that proves the cross-file overlay works at all, and it is deliberately run
//! against the REAL binary, end to end: two processes, the same on-disk project,
//! the same cwd — no in-process shortcut that could accidentally compare the
//! overlay against itself.
//!
//! The fixture is chosen so cross-file context CHANGES the answer:
//! `Base#helper` is public in one file and `Sub` overrides it as private in
//! another, so `def.override-visibility-reduced` fires — but only when both files
//! are in the index. `rigor check lib/sub.rb` (single file) is silent; that
//! silence is asserted too, so a regression to the pre-S4b single-file LSP index
//! fails this test rather than passing vacuously.
//!
//! Hermetic: `RIGOR_NO_RUBY=1` puts BOTH processes in the Ruby-free sound subset,
//! so no sidecar availability difference can move either side's findings.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

/// A self-cleaning unique temp directory (no external crate dependency).
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "rigor-{tag}-{}-{n}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(path.join("lib")).expect("create temp project");
        // macOS's temp dir is a symlink (`/var` → `/private/var`); canonicalize so
        // the `file:` URIs we send match the paths the server canonicalizes.
        TempDir(fs::canonicalize(&path).expect("canonicalize temp project"))
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

const BASE_RB: &str = "class Base\n  def helper\n  end\nend\n";
const SUB_RB: &str = "class Sub < Base\n  private\n\n  def helper\n  end\nend\n";

/// A Rails-shaped `Gemfile.lock`: activesupport locked as a direct spec, which
/// ADR-72's `bundler.auto_detect` turns into the `activesupport-core-ext` plugin
/// overlay. `check` has always applied it (`Config::effective_plugins`); review N1
/// found the LSP building its `CoreIndex` from the bare `plugins:` instead.
const GEMFILE_LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    \
                            activesupport (7.1.3)\n      concurrent-ruby (~> 1.0)\n    \
                            concurrent-ruby (1.2.3)\n\nPLATFORMS\n  ruby\n\n\
                            DEPENDENCIES\n  activesupport\n\nBUNDLED WITH\n   2.5.6\n";

/// `blank?` is an activesupport core-ext on `String` — undefined in plain core
/// RBS. With the overlay applied it resolves; without it, it is an
/// `undefined method` error.
const BLANK_RB: &str = "s = \"hello\"\ns.blank?\n";

/// A project RBS declaring an explicit `-> void` return — the only way to make
/// `static.value-use.void` (the ADR-100 bleeding-edge rule) fire.
const WIDGET_RBS: &str = "class Widget\n  def fire: () -> void\nend\n";

/// Uses that `-> void` return as a value: the rule's assignment-RHS context.
const VOID_USE_RB: &str = "w = Widget.new\nx = w.fire\n";

/// Run `rigor <args>` in `cwd` under the hermetic Ruby-free posture, returning
/// stdout.
fn rigor_check(cwd: &Path, args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_rigor"))
        .current_dir(cwd)
        .env("RIGOR_NO_RUBY", "1")
        .env_remove("RIGOR_RUBY")
        .args(args)
        .output()
        .expect("run rigor");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// One diagnostic in the comparable shape: `(1-based line, 1-based column,
/// severity word, rule id, message)`.
///
/// The rule id is part of the tuple because the stage-3 parity slice compares
/// SEVERITIES: a comparison keyed only on position + message could be satisfied
/// by the wrong rule at the same span, and "rule id + line + column + severity"
/// is the shape that acceptance asks for.
type Finding = (u32, u32, String, String, String);

/// `rigor check --format json <args…>` in `cwd`, keeping only the rows whose
/// `path` is `file`. JSON rather than the text renderer because only the JSON
/// shape carries the `rule` id.
fn check_findings(cwd: &Path, args: &[&str], file: &str) -> Vec<Finding> {
    let mut argv = vec!["check", "--format", "json"];
    argv.extend_from_slice(args);
    let stdout = rigor_check(cwd, &argv);
    let rows: Vec<serde_json::Value> =
        serde_json::from_str(stdout.trim()).unwrap_or_else(|e| panic!("parse {stdout:?}: {e}"));
    rows.iter()
        .filter(|r| r["path"].as_str() == Some(file))
        .map(|r| {
            (
                r["line"].as_u64().unwrap() as u32,
                r["column"].as_u64().unwrap() as u32,
                r["severity"].as_str().unwrap().to_string(),
                r["rule"].as_str().unwrap().to_string(),
                r["message"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

/// A live `rigor lsp --transport=stdio` child, spoken to over JSON-RPC framing.
struct LspChild {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    stdout: BufReader<std::process::ChildStdout>,
}

impl LspChild {
    fn spawn(cwd: &Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_rigor"))
            .current_dir(cwd)
            .env("RIGOR_NO_RUBY", "1")
            .env_remove("RIGOR_RUBY")
            .args(["lsp", "--transport=stdio"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn rigor lsp");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Self { child, stdin, stdout }
    }

    fn send(&mut self, msg: serde_json::Value) {
        let body = serde_json::to_string(&msg).unwrap();
        write!(self.stdin, "Content-Length: {}\r\n\r\n{body}", body.len()).unwrap();
        self.stdin.flush().unwrap();
    }

    /// Read one framed JSON-RPC message.
    fn recv(&mut self) -> serde_json::Value {
        let mut len = 0usize;
        loop {
            let mut line = String::new();
            let n = self.stdout.read_line(&mut line).expect("read header");
            assert!(n > 0, "the server closed its stdout mid-message");
            let trimmed = line.trim_end();
            if trimmed.is_empty() {
                break; // end of headers
            }
            if let Some(v) = trimmed.strip_prefix("Content-Length: ") {
                len = v.parse().unwrap();
            }
        }
        let mut buf = vec![0u8; len];
        self.stdout.read_exact(&mut buf).expect("read body");
        serde_json::from_slice(&buf).expect("parse body")
    }

    /// Read until the `publishDiagnostics` for `uri` arrives, skipping the
    /// startup posture `window/showMessage` and anything else in between.
    fn recv_diagnostics(&mut self, uri: &str) -> Vec<serde_json::Value> {
        for _ in 0..50 {
            let msg = self.recv();
            if msg.get("method").and_then(|m| m.as_str())
                == Some("textDocument/publishDiagnostics")
                && msg["params"]["uri"].as_str() == Some(uri)
            {
                return msg["params"]["diagnostics"].as_array().unwrap().clone();
            }
        }
        panic!("no publishDiagnostics for {uri} within 50 messages");
    }

    fn shutdown(mut self) {
        self.send(serde_json::json!({
            "jsonrpc": "2.0", "id": 99, "method": "shutdown", "params": null
        }));
        // Drain until the shutdown response, then exit.
        for _ in 0..50 {
            let m = self.recv();
            if m.get("id").and_then(serde_json::Value::as_i64) == Some(99) {
                break;
            }
        }
        self.send(serde_json::json!({
            "jsonrpc": "2.0", "method": "exit", "params": null
        }));
        let status = self.child.wait().expect("wait for rigor lsp");
        assert!(status.success(), "rigor lsp exited with {status}");
    }
}

#[test]
fn lsp_saved_buffer_diagnostics_equal_project_check_diagnostics() {
    let dir = TempDir::new("lsp-parity");
    fs::write(dir.path().join("lib/base.rb"), BASE_RB).unwrap();
    fs::write(dir.path().join("lib/sub.rb"), SUB_RB).unwrap();

    // (1) The single-file answer: SILENT. The fixture's finding is purely
    // cross-file, so a pre-S4b (single-file-index) LSP fails the comparison below.
    let single = check_findings(dir.path(), &["lib/sub.rb"], "lib/sub.rb");
    assert!(single.is_empty(), "single-file `check` must be silent for this fixture: {single:?}");

    // (2) The project answer: the override-visibility finding.
    let expected = check_findings(dir.path(), &["lib"], "lib/sub.rb");
    assert_eq!(expected.len(), 1, "project `check` reports the cross-file finding: {expected:?}");

    // (3) The LSP's answer for the same, SAVED (never edited) buffer.
    let actual = lsp_findings(dir.path(), "lib/sub.rb", SUB_RB);

    assert_eq!(
        actual, expected,
        "LSP diagnostics for a saved buffer must equal `rigor check`'s project-wide \
         diagnostics for that file (line, column, severity, message)"
    );
}

#[test]
fn lsp_applies_the_same_gemfile_lock_plugin_overlays_as_check() {
    // Review N1: `check` builds its `CoreIndex` from `Config::effective_plugins`
    // (config `plugins:` PLUS the ADR-72 `Gemfile.lock`-gated auto-detected
    // overlays); the LSP passed the bare `cfg.plugins`. On the most common Ruby
    // project shape — a Gemfile.lock with activesupport — the editor therefore
    // fired `undefined method 'blank?' for "hello"` on code `rigor check` accepts.
    // A per-keystroke false positive, and a direct contradiction of the S4b parity
    // headline, so it is pinned end to end like the headline itself.
    let dir = TempDir::new("lsp-plugins");
    fs::write(dir.path().join("Gemfile.lock"), GEMFILE_LOCK).unwrap();
    fs::write(dir.path().join("lib/blank.rb"), BLANK_RB).unwrap();

    // `check` is silent: the activesupport overlay reopens `String` with `blank?`.
    let expected = check_findings(dir.path(), &["lib"], "lib/blank.rb");
    assert!(
        expected.is_empty(),
        "`check` resolves `blank?` via the Gemfile.lock overlay: {expected:?}"
    );

    // The LSP must agree — and, as everywhere in this file, the comparison is an
    // equality against `check`, not a hardcoded expectation.
    let actual = lsp_findings(dir.path(), "lib/blank.rb", BLANK_RB);
    assert_eq!(
        actual, expected,
        "the LSP must apply the same Gemfile.lock plugin overlays `check` does"
    );
}

// ---------------------------------------------------------------------------
// Stage-3 parity tail: the ADR-8 SeverityStamp + the bleeding-edge rule gate.
//
// `check`'s stage 3 ends by re-stamping each diagnostic's severity from the
// profile + user + bleeding-edge overrides and DROPPING an `:off` resolution; it
// also gates the `static.value-use.void` collector on the same resolution. The
// LSP did neither, so a project on a non-default `severity_profile:` /
// `severity_overrides:` saw a different rule SET and different levels in the
// editor than its CI run reported. Each test below asserts LSP == `check` and
// carries its own non-vacuity control in the same fixture.
// ---------------------------------------------------------------------------

/// A rule the config turns OFF is absent from BOTH tools — a PRESENCE parity, not
/// merely a severity one. The control run (same fixture, no `.rigor.yml`) proves
/// the rule fires at all, so the empty-vs-empty comparison cannot pass vacuously.
#[test]
fn lsp_drops_an_off_rule_exactly_as_check_does() {
    let dir = TempDir::new("lsp-sev-off");
    fs::write(dir.path().join("lib/base.rb"), BASE_RB).unwrap();
    fs::write(dir.path().join("lib/sub.rb"), SUB_RB).unwrap();

    // CONTROL — no config: both tools report the one cross-file finding.
    let control_check = check_findings(dir.path(), &["lib"], "lib/sub.rb");
    assert_eq!(control_check.len(), 1, "the fixture fires unconfigured: {control_check:?}");
    assert_eq!(control_check[0].3, "def.override-visibility-reduced");
    assert_eq!(lsp_findings(dir.path(), "lib/sub.rb", SUB_RB), control_check);

    // …now turn that exact rule off.
    fs::write(
        dir.path().join(".rigor.yml"),
        "severity_overrides:\n  def.override-visibility-reduced: off\n",
    )
    .unwrap();

    let expected = check_findings(dir.path(), &["lib"], "lib/sub.rb");
    assert!(expected.is_empty(), "`check` drops an `off` rule: {expected:?}");
    assert_eq!(
        lsp_findings(dir.path(), "lib/sub.rb", SUB_RB),
        expected,
        "an `off` rule must publish NOTHING to the editor, exactly as `check` reports nothing"
    );
}

/// Under a non-default `severity_profile:` the LSP's published severities equal
/// `check`'s, per (rule id, line, column, severity). Non-vacuous by construction:
/// the profile moves this rule's severity AWAY from its authored one, so the
/// pre-stamp LSP (which published the authored severity) disagrees.
#[test]
fn lsp_publishes_the_profile_resolved_severity_like_check() {
    let dir = TempDir::new("lsp-sev-profile");
    fs::write(dir.path().join("lib/base.rb"), BASE_RB).unwrap();
    fs::write(dir.path().join("lib/sub.rb"), SUB_RB).unwrap();

    // CONTROL — the AUTHORED severity, as published with no profile configured.
    let authored = check_findings(dir.path(), &["lib"], "lib/sub.rb");
    assert_eq!(authored.len(), 1);
    assert_eq!(authored[0].2, "warning", "authored/balanced level: {authored:?}");

    // `strict` re-stamps `def.override-visibility-reduced` warning → error.
    fs::write(dir.path().join(".rigor.yml"), "severity_profile: strict\n").unwrap();
    let expected = check_findings(dir.path(), &["lib"], "lib/sub.rb");
    assert_eq!(expected.len(), 1, "the rule still fires under `strict`: {expected:?}");
    assert_eq!(
        expected[0].2, "error",
        "the control: `strict` MOVES the severity, so this comparison discriminates"
    );

    assert_eq!(
        lsp_findings(dir.path(), "lib/sub.rb", SUB_RB),
        expected,
        "the editor's severity must be the profile-RESOLVED one, not the authored one"
    );
}

/// The bleeding-edge `static.value-use.void` rule runs in the editor exactly when
/// it runs in `check`: not at all by default (every shipped profile has it
/// `:off`), and on both sides once `bleeding_edge:` adopts the feature.
#[test]
fn lsp_runs_the_bleeding_edge_void_rule_exactly_when_check_does() {
    let dir = TempDir::new("lsp-bleeding");
    fs::create_dir_all(dir.path().join("sig")).unwrap();
    fs::write(dir.path().join("sig/widget.rbs"), WIDGET_RBS).unwrap();
    fs::write(dir.path().join("lib/void_use.rb"), VOID_USE_RB).unwrap();

    // DEFAULT: the feature is off, so neither tool reports the rule.
    let off_check = check_findings(dir.path(), &["lib"], "lib/void_use.rb");
    assert!(off_check.is_empty(), "the void rule is off by default: {off_check:?}");
    assert_eq!(
        lsp_findings(dir.path(), "lib/void_use.rb", VOID_USE_RB),
        off_check,
        "the LSP must not publish a rule `check` does not run"
    );

    // ADOPTED: both tools report it, at the same place and level.
    fs::write(dir.path().join(".rigor.yml"), "bleeding_edge:\n  - use-of-void-value\n").unwrap();
    let expected = check_findings(dir.path(), &["lib"], "lib/void_use.rb");
    assert_eq!(expected.len(), 1, "the control: the feature turns the rule ON: {expected:?}");
    assert_eq!(expected[0].3, "static.value-use.void");
    assert_eq!(
        lsp_findings(dir.path(), "lib/void_use.rb", VOID_USE_RB),
        expected,
        "an adopted bleeding-edge rule must reach the editor too"
    );
}

/// A single-file finding (`call.undefined-method`) — the non-excluded sibling's
/// diagnostic, independent of any cross-file context so excluding another file
/// cannot move it.
const TYPO_RB: &str = "s = \"hi\"\ns.lenght\n";

/// Config `exclude:` is `check`'s STAGE-1 file filter: an excluded file is never
/// even read, so `check` reports no rows for it. The LSP published markers for the
/// open buffer anyway — a PRESENCE mismatch of exactly the class the stage-3 slice
/// closed for `severity: off`.
///
/// The fixture carries both controls in one project: `lib/sub.rb` (the excluded
/// file) provably fires when it is NOT excluded, and `lib/typo.rb` (never
/// excluded) provably still fires when it IS — so neither the empty-vs-empty
/// comparison nor the "not over-broad" claim can pass vacuously.
#[test]
fn lsp_honours_config_exclude_exactly_as_check_does() {
    let dir = TempDir::new("lsp-exclude");
    fs::write(dir.path().join("lib/base.rb"), BASE_RB).unwrap();
    fs::write(dir.path().join("lib/sub.rb"), SUB_RB).unwrap();
    fs::write(dir.path().join("lib/typo.rb"), TYPO_RB).unwrap();

    // CONTROL — no config: both files fire, in both tools.
    let control_sub = check_findings(dir.path(), &["lib"], "lib/sub.rb");
    assert_eq!(control_sub.len(), 1, "the fixture fires unexcluded: {control_sub:?}");
    assert_eq!(control_sub[0].3, "def.override-visibility-reduced");
    assert_eq!(lsp_findings(dir.path(), "lib/sub.rb", SUB_RB), control_sub);

    let control_typo = check_findings(dir.path(), &["lib"], "lib/typo.rb");
    assert_eq!(control_typo.len(), 1, "the sibling fires too: {control_typo:?}");
    assert_eq!(lsp_findings(dir.path(), "lib/typo.rb", TYPO_RB), control_typo);

    // …now exclude ONE of them. The pattern is spelled the way `check` matches it:
    // bare `check` expands `paths: ["lib"]` to `lib/sub.rb` and applies
    // `cfg.is_excluded` to that string (`main.rs` stage 1).
    fs::write(dir.path().join(".rigor.yml"), "exclude:\n  - \"lib/sub.rb\"\n").unwrap();

    // (1) The excluded buffer: `check` reports nothing, and the LSP publishes an
    // EMPTY set — which is also what clears any markers the editor already holds.
    let expected = check_findings(dir.path(), &["lib"], "lib/sub.rb");
    assert!(expected.is_empty(), "`check` never reads an excluded file: {expected:?}");
    assert_eq!(
        lsp_findings(dir.path(), "lib/sub.rb", SUB_RB),
        expected,
        "an `exclude:`d buffer must publish NOTHING, exactly as `check` reports nothing"
    );

    // (2) The non-excluded sibling is untouched — the filter is not over-broad.
    let sibling = check_findings(dir.path(), &["lib"], "lib/typo.rb");
    assert_eq!(sibling.len(), 1, "the control: the sibling still fires under `check`");
    assert_eq!(
        lsp_findings(dir.path(), "lib/typo.rb", TYPO_RB),
        sibling,
        "excluding one file must not silence another"
    );
}

// ---------------------------------------------------------------------------
// PR #45 review — the three path forms the first cut of the `exclude:` gate
// silently DROPPED while `check` analyses them. Each is a measured regression
// probe: `check` is asked first and the LSP must equal it.
//
// The invariant they pin: a buffer is excluded iff EVERY discovery spelling of
// that file is excluded. One file reaches discovery under several names — a
// symlinked `.rb` under the LINK's name (`collect_rb_files` includes symlinked
// files on purpose, matching `Dir.glob`), overlapping `paths:` roots under two —
// and `check` analyses it if ANY of them survives `exclude:`.
// ---------------------------------------------------------------------------

/// B1 — a symlinked `.rb` inside `paths:` pointing at an EXCLUDED tree.
/// Discovery walks `lib/shared.rb` (not excluded by `**/vendor/**`), so `check`
/// analyses it; the buffer's canonical path is `vendor/shared.rb`, which is.
#[test]
fn lsp_analyses_a_symlinked_file_whose_target_is_excluded() {
    let dir = TempDir::new("lsp-exclude-b1");
    fs::create_dir_all(dir.path().join("vendor")).unwrap();
    fs::write(dir.path().join("vendor/shared.rb"), TYPO_RB).unwrap();
    std::os::unix::fs::symlink(
        dir.path().join("vendor/shared.rb"),
        dir.path().join("lib/shared.rb"),
    )
    .unwrap();
    fs::write(dir.path().join(".rigor.yml"), "exclude:\n  - \"**/vendor/**\"\n").unwrap();

    let expected = check_findings(dir.path(), &["lib"], "lib/shared.rb");
    assert_eq!(
        expected.len(),
        1,
        "the control: discovery walks the LINK's name, which `**/vendor/**` does not \
         cover, so `check` analyses it: {expected:?}"
    );
    assert_eq!(
        lsp_findings(dir.path(), "lib/shared.rb", TYPO_RB),
        expected,
        "B1: the LSP must not drop a file `check` analyses under its link name"
    );
}

/// B2 — a symlink inside `paths:` whose TARGET's own spelling is excluded.
/// Discovery keeps `lib/link.rb`, so `check` reports the content under that name.
#[test]
fn lsp_analyses_a_symlink_whose_target_spelling_is_excluded() {
    let dir = TempDir::new("lsp-exclude-b2");
    fs::write(dir.path().join("lib/real.rb"), TYPO_RB).unwrap();
    std::os::unix::fs::symlink(dir.path().join("lib/real.rb"), dir.path().join("lib/link.rb"))
        .unwrap();
    fs::write(dir.path().join(".rigor.yml"), "exclude:\n  - \"lib/real.rb\"\n").unwrap();

    let expected = check_findings(dir.path(), &["lib"], "lib/link.rb");
    assert_eq!(
        expected.len(),
        1,
        "the control: `lib/link.rb` survives `exclude:`, so `check` analyses the \
         content under that name: {expected:?}"
    );
    // …and the target's own name reports nothing, which is what makes the two
    // spellings genuinely disagree.
    assert!(check_findings(dir.path(), &["lib"], "lib/real.rb").is_empty());
    assert_eq!(
        lsp_findings(dir.path(), "lib/link.rb", TYPO_RB),
        expected,
        "B2: the buffer opened under the surviving name must still be analysed"
    );
}

/// B3 — OVERLAPPING `paths:` roots. Discovery yields both `./lib/a.rb` (pruned by
/// `./lib/**`) and `lib/a.rb` (kept), so `check` analyses the file. The first cut
/// returned on the first containing root, making the answer depend on config order
/// — so both orders are driven.
#[test]
fn lsp_analyses_a_file_kept_by_one_of_two_overlapping_roots() {
    for (tag, order) in [
        ("lsp-exclude-b3a", "paths:\n  - \".\"\n  - lib\n"),
        ("lsp-exclude-b3b", "paths:\n  - lib\n  - \".\"\n"),
    ] {
        let dir = TempDir::new(tag);
        fs::write(dir.path().join("lib/a.rb"), TYPO_RB).unwrap();
        fs::write(
            dir.path().join(".rigor.yml"),
            format!("{order}exclude:\n  - \"./lib/**\"\n"),
        )
        .unwrap();

        // Bare `check` (no path args) — the invocation `paths:` governs.
        let expected = check_findings(dir.path(), &[], "lib/a.rb");
        assert_eq!(
            expected.len(),
            1,
            "[{order:?}] the control: the `lib` root's spelling survives, so `check` \
             analyses the file: {expected:?}"
        );
        assert_eq!(
            lsp_findings(dir.path(), "lib/a.rb", TYPO_RB),
            expected,
            "[{order:?}] B3: one surviving root spelling is enough — and the answer \
             must not depend on `paths:` order"
        );
    }
}

/// Boot `rigor lsp` in `root`, `didOpen` `rel` with `text`, and return its
/// published diagnostics in the same comparable shape [`check_findings`] yields.
fn lsp_findings(root: &Path, rel: &str, text: &str) -> Vec<Finding> {
    let uri = format!("file://{}", root.join(rel).display());
    let mut lsp = LspChild::spawn(root);
    lsp.send(serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": { "capabilities": {} }
    }));
    lsp.recv(); // initialize response
    lsp.send(serde_json::json!({
        "jsonrpc": "2.0", "method": "initialized", "params": {}
    }));
    lsp.send(serde_json::json!({
        "jsonrpc": "2.0", "method": "textDocument/didOpen",
        "params": { "textDocument": {
            "uri": uri, "languageId": "ruby", "version": 1, "text": text
        }}
    }));
    let diags = lsp.recv_diagnostics(&uri);
    let findings = diags
        .iter()
        .map(|d| {
            let line = d["range"]["start"]["line"].as_u64().unwrap() as u32 + 1;
            let col = d["range"]["start"]["character"].as_u64().unwrap() as u32 + 1;
            let sev = match d["severity"].as_u64().unwrap() {
                1 => "error",
                2 => "warning",
                _ => "info",
            };
            (
                line,
                col,
                sev.to_string(),
                d["code"].as_str().unwrap().to_string(),
                d["message"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    lsp.shutdown();
    findings
}
