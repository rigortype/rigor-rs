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
/// severity word, message)`.
type Finding = (u32, u32, String, String);

/// Parse `check`'s text output, keeping only the rows for `file` (a path suffix).
/// Row grammar: `path:line:col: severity: message`.
fn check_findings(stdout: &str, file: &str) -> Vec<Finding> {
    stdout
        .lines()
        .filter(|l| l.starts_with(file))
        .map(|l| {
            let rest = &l[file.len() + 1..]; // past "path:"
            let mut it = rest.splitn(3, ':');
            let line: u32 = it.next().unwrap().parse().unwrap();
            let col: u32 = it.next().unwrap().parse().unwrap();
            let tail = it.next().unwrap().trim(); // "severity: message"
            let (sev, msg) = tail.split_once(": ").unwrap();
            (line, col, sev.trim().to_string(), msg.trim().to_string())
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
    let single = rigor_check(dir.path(), &["check", "lib/sub.rb"]);
    assert!(
        check_findings(&single, "lib/sub.rb").is_empty(),
        "single-file `check` must be silent for this fixture: {single}"
    );

    // (2) The project answer: the override-visibility finding.
    let project = rigor_check(dir.path(), &["check", "lib"]);
    let expected = check_findings(&project, "lib/sub.rb");
    assert_eq!(expected.len(), 1, "project `check` reports the cross-file finding: {project}");

    // (3) The LSP's answer for the same, SAVED (never edited) buffer.
    let uri = format!("file://{}", dir.path().join("lib/sub.rb").display());
    let mut lsp = LspChild::spawn(dir.path());
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
            "uri": uri, "languageId": "ruby", "version": 1, "text": SUB_RB
        }}
    }));
    let diags = lsp.recv_diagnostics(&uri);
    let actual: Vec<Finding> = diags
        .iter()
        .map(|d| {
            let line = d["range"]["start"]["line"].as_u64().unwrap() as u32 + 1;
            let col = d["range"]["start"]["character"].as_u64().unwrap() as u32 + 1;
            let sev = match d["severity"].as_u64().unwrap() {
                1 => "error",
                2 => "warning",
                _ => "info",
            };
            (line, col, sev.to_string(), d["message"].as_str().unwrap().to_string())
        })
        .collect();

    assert_eq!(
        actual, expected,
        "LSP diagnostics for a saved buffer must equal `rigor check`'s project-wide \
         diagnostics for that file (line, column, severity, message)"
    );
    lsp.shutdown();
}
