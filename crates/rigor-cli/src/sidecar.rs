//! ADR-0008 / ADR-0036 — the Ruby sidecar client.
//!
//! The sidecar is the project's Ruby running [`SIDECAR_RB`] as a request loop
//! (newline-delimited JSON over stdin/stdout). This module owns spawning it, the
//! **availability probe** ADR-0036 gates the coverage posture on, and a
//! **persistent worker** ([`Sidecar`]) that executes purity-gated constant folds
//! rigor does not reimplement natively (ADR-0008). Foldability is decided in Rust
//! (`ruby_fold` (Slice 2b)); this only *executes* a call rigor already approved.
//!
//! A present-but-broken Ruby (version skew, missing stdlib) fails the handshake
//! and counts as unavailable — the "handshake, not a bare `ruby` on PATH" rule.
//! Spawn + handshake is timeout-guarded so a hung ruby never blocks the run.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use rigor_types::Scalar;

use crate::ruby_mode::RubyMode;

/// The embedded sidecar script — shipped in the binary (`include_str!`) so the
/// single-artifact distribution is preserved (no external file to locate).
pub const SIDECAR_RB: &str = include_str!("sidecar.rb");

/// The protocol version the client understands; must match the sidecar's
/// `rigor_sidecar` handshake field.
const PROTOCOL_VERSION: u64 = 1;

/// How long to wait for spawn + handshake before declaring the sidecar
/// unavailable, so a hung or wrong-version ruby never blocks the run.
const SPAWN_TIMEOUT: Duration = Duration::from_secs(5);

/// A successful handshake.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Handshake {
    /// The sidecar ruby's `RUBY_VERSION`.
    pub ruby_version: String,
}

/// Why the sidecar could not be brought up. All map to "run the sound subset"
/// (or, once `require` grows teeth, the exit-69 hard error).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeError {
    /// The ruby binary could not be spawned (not found / not executable).
    Spawn(String),
    /// No usable handshake line (EOF, non-JSON, wrong protocol version).
    Handshake,
    /// Spawn + handshake did not complete within [`SPAWN_TIMEOUT`].
    Timeout,
}

impl std::fmt::Display for ProbeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProbeError::Spawn(e) => write!(f, "could not spawn ruby ({e})"),
            ProbeError::Handshake => f.write_str("no usable sidecar handshake"),
            ProbeError::Timeout => f.write_str("sidecar spawn timed out"),
        }
    }
}

/// The ruby binary a mode would spawn, or `None` when the mode opts out (`off`).
/// `Require`/`Auto` use `ruby` on PATH (project-Ruby / bundler detection is a
/// later slice); `Path(p)` names the binary explicitly.
#[must_use]
pub fn ruby_bin_for(mode: &RubyMode) -> Option<String> {
    match mode {
        RubyMode::Off => None,
        RubyMode::Path(p) => Some(p.clone()),
        RubyMode::Require | RubyMode::Auto => Some("ruby".to_string()),
    }
}

/// A live sidecar worker: the child process plus its framed stdio. Kept alive
/// across folds (lazily spawned once). `fold` is `&mut` — the worker is a serial
/// request/response channel; a caller sharing it across threads guards it (e.g.
/// `Mutex<Sidecar>`).
pub struct Sidecar {
    child: Child,
    stdin: ChildStdin,
    reader: BufReader<ChildStdout>,
    ruby_version: String,
    /// Set once a request hits an IO/protocol error, so later folds short-circuit
    /// to `None` instead of retrying a broken pipe.
    dead: bool,
}

impl Sidecar {
    /// Spawn `ruby_bin` running the sidecar and complete the handshake. The
    /// spawn+handshake read runs on a worker thread with a timeout; the stream
    /// handles are handed back so the returned worker owns them for later folds.
    pub fn spawn(ruby_bin: &str) -> Result<Sidecar, ProbeError> {
        let mut child = Command::new(ruby_bin)
            .arg("-e")
            .arg(SIDECAR_RB)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| ProbeError::Spawn(e.to_string()))?;

        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(read_handshake(stdin, stdout));
        });

        match rx.recv_timeout(SPAWN_TIMEOUT) {
            Ok(Ok((ruby_version, stdin, reader))) => {
                Ok(Sidecar { child, stdin, reader, ruby_version, dead: false })
            }
            Ok(Err(e)) => {
                let _ = child.kill();
                let _ = child.wait();
                Err(e)
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                Err(ProbeError::Timeout)
            }
        }
    }

    /// The sidecar ruby's version string.
    #[must_use]
    pub fn ruby_version(&self) -> &str {
        &self.ruby_version
    }

    /// Execute one purity-gated fold: `recv.method(*args)` on scalar literals.
    /// Returns the resulting [`Scalar`], or `None` when the sidecar declined
    /// (non-scalar / raised / non-finite) or the worker is dead. The caller must
    /// have already confirmed the `(class, method)` is `ruby_fold` (Slice 2b)-safe.
    pub fn fold(&mut self, recv: &Scalar, method: &str, args: &[Scalar]) -> Option<Scalar> {
        if self.dead {
            return None;
        }
        let req = serde_json::json!({
            "op": "fold",
            "recv": scalar_to_json(recv),
            "method": method,
            "args": args.iter().map(scalar_to_json).collect::<Vec<_>>(),
        });
        match self.request(&req) {
            Some(reply) if reply.get("ok").and_then(serde_json::Value::as_bool) == Some(true) => {
                reply.get("result").and_then(json_to_scalar)
            }
            Some(_) => None, // explicit decline
            None => None,    // IO error (worker now marked dead)
        }
    }

    /// Send `{"op":"ping"}` and confirm the `{"ok":true}` reply.
    pub fn ping(&mut self) -> bool {
        let req = serde_json::json!({ "op": "ping" });
        self.request(&req)
            .and_then(|r| r.get("ok").and_then(serde_json::Value::as_bool))
            == Some(true)
    }

    /// One request/response round trip. On any IO/parse failure the worker is
    /// marked dead and `None` is returned.
    fn request(&mut self, req: &serde_json::Value) -> Option<serde_json::Value> {
        if self.dead {
            return None;
        }
        let mut line = serde_json::to_string(req).ok()?;
        line.push('\n');
        if self.stdin.write_all(line.as_bytes()).is_err() || self.stdin.flush().is_err() {
            self.dead = true;
            return None;
        }
        let mut reply = String::new();
        if self.reader.read_line(&mut reply).unwrap_or(0) == 0 {
            self.dead = true;
            return None;
        }
        match serde_json::from_str(reply.trim()) {
            Ok(v) => Some(v),
            Err(_) => {
                self.dead = true;
                None
            }
        }
    }
}

impl Drop for Sidecar {
    fn drop(&mut self) {
        // Best-effort graceful shutdown, then ensure the child is reaped.
        let _ = self.stdin.write_all(b"{\"op\":\"shutdown\"}\n");
        let _ = self.stdin.flush();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A [`rigor_infer::RubyFolder`] backed by a live [`Sidecar`], with a per-run
/// memo. Wraps the worker in a `Mutex` so one folder is shared across the
/// file-parallel analysis (the worker is a serial channel); the memo collapses
/// repeated `(recv, method, args)` folds to a single round trip.
pub struct SidecarFolder {
    state: std::sync::Mutex<FolderState>,
}

struct FolderState {
    sidecar: Sidecar,
    memo: std::collections::HashMap<String, Option<Scalar>>,
}

impl SidecarFolder {
    /// Wrap a spawned [`Sidecar`] as a folder.
    #[must_use]
    pub fn new(sidecar: Sidecar) -> Self {
        SidecarFolder {
            state: std::sync::Mutex::new(FolderState {
                sidecar,
                memo: std::collections::HashMap::new(),
            }),
        }
    }

}

impl rigor_infer::RubyFolder for SidecarFolder {
    fn fold(&self, recv: &Scalar, method: &str, args: &[Scalar]) -> Option<Scalar> {
        // A memo key over the pinned inputs. `Scalar` isn't `Hash` (it carries an
        // `f64`), so key on its `Debug` form — sufficient and deterministic for a
        // pure-fold cache. `\x1f` (unit separator) can't appear in the parts.
        let key = format!("{recv:?}\x1f{method}\x1f{args:?}");
        let mut st = self.state.lock().ok()?;
        if let Some(cached) = st.memo.get(&key) {
            return cached.clone();
        }
        let result = st.sidecar.fold(recv, method, args);
        st.memo.insert(key, result.clone());
        result
    }
}

/// Spawn the sidecar, confirm the handshake AND one `ping` round trip, then shut
/// it down — the ADR-0036 availability probe. Side-effect free; returns the
/// [`Handshake`] on success.
pub fn probe(ruby_bin: &str) -> Result<Handshake, ProbeError> {
    let mut sidecar = Sidecar::spawn(ruby_bin)?;
    if !sidecar.ping() {
        return Err(ProbeError::Handshake);
    }
    Ok(Handshake { ruby_version: sidecar.ruby_version().to_string() })
}

/// Read the handshake line off the child's stdout, verify the protocol version,
/// and hand the stream halves back so the caller can keep the worker alive.
#[allow(clippy::type_complexity)]
fn read_handshake(
    stdin: Option<ChildStdin>,
    stdout: Option<ChildStdout>,
) -> Result<(String, ChildStdin, BufReader<ChildStdout>), ProbeError> {
    let (stdin, stdout) = match (stdin, stdout) {
        (Some(i), Some(o)) => (i, o),
        _ => return Err(ProbeError::Handshake),
    };
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    if reader.read_line(&mut line).unwrap_or(0) == 0 {
        return Err(ProbeError::Handshake);
    }
    let parsed: serde_json::Value =
        serde_json::from_str(line.trim()).map_err(|_| ProbeError::Handshake)?;
    if parsed.get("rigor_sidecar").and_then(serde_json::Value::as_u64) != Some(PROTOCOL_VERSION) {
        return Err(ProbeError::Handshake);
    }
    let ruby_version = parsed
        .get("ruby_version")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    Ok((ruby_version, stdin, reader))
}

/// Encode a [`Scalar`] as the tagged JSON the sidecar decodes.
fn scalar_to_json(s: &Scalar) -> serde_json::Value {
    use serde_json::json;
    match s {
        Scalar::Int(i) => json!({ "t": "int", "v": i }),
        Scalar::Float(f) => json!({ "t": "float", "v": f }),
        Scalar::Str(s) => json!({ "t": "str", "v": s }),
        Scalar::Sym(s) => json!({ "t": "sym", "v": s }),
        Scalar::Bool(b) => json!({ "t": "bool", "v": b }),
        Scalar::Nil => json!({ "t": "nil" }),
    }
}

/// Decode a tagged JSON scalar back to a [`Scalar`]; `None` if malformed.
fn json_to_scalar(v: &serde_json::Value) -> Option<Scalar> {
    match v.get("t")?.as_str()? {
        "int" => v.get("v")?.as_i64().map(Scalar::Int),
        "float" => v.get("v")?.as_f64().map(Scalar::Float),
        "str" => v.get("v")?.as_str().map(|s| Scalar::Str(s.to_string())),
        "sym" => v.get("v")?.as_str().map(|s| Scalar::Sym(s.to_string())),
        "bool" => v.get("v")?.as_bool().map(Scalar::Bool),
        "nil" => Some(Scalar::Nil),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ruby_on_path() -> bool {
        Command::new("ruby")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    #[test]
    fn ruby_bin_selection() {
        assert_eq!(ruby_bin_for(&RubyMode::Off), None);
        assert_eq!(ruby_bin_for(&RubyMode::Require).as_deref(), Some("ruby"));
        assert_eq!(ruby_bin_for(&RubyMode::Auto).as_deref(), Some("ruby"));
        assert_eq!(
            ruby_bin_for(&RubyMode::Path("/opt/ruby".into())).as_deref(),
            Some("/opt/ruby")
        );
    }

    #[test]
    fn probe_absent_ruby_is_spawn_error() {
        let err = probe("this-ruby-does-not-exist-xyzzy").unwrap_err();
        assert!(matches!(err, ProbeError::Spawn(_)), "got {err:?}");
    }

    #[test]
    fn probe_real_ruby_handshakes_and_pings() {
        if !ruby_on_path() {
            eprintln!("skipping: no ruby on PATH");
            return;
        }
        let hs = probe("ruby").expect("real ruby should handshake");
        assert!(hs.ruby_version.chars().next().is_some_and(|c| c.is_ascii_digit()));
    }

    #[test]
    fn fold_real_ruby_executes_gated_calls() {
        if !ruby_on_path() {
            eprintln!("skipping: no ruby on PATH");
            return;
        }
        let mut sc = Sidecar::spawn("ruby").expect("spawn");
        // Integer#to_s(base) — sidecar territory (Rust core is base-10 only).
        assert_eq!(
            sc.fold(&Scalar::Int(255), "to_s", &[Scalar::Int(16)]),
            Some(Scalar::Str("ff".into()))
        );
        // String#% (format) — a pure, deterministic long-tail fold.
        assert_eq!(
            sc.fold(&Scalar::Str("%05d".into()), "%", &[Scalar::Int(42)]),
            Some(Scalar::Str("00042".into()))
        );
        // A sampling of the expanded allowlist (all real-Ruby, parity-confirmed).
        assert_eq!(
            sc.fold(&Scalar::Float(3.14159), "round", &[Scalar::Int(2)]),
            Some(Scalar::Float(3.14))
        );
        assert_eq!(sc.fold(&Scalar::Int(12), "gcd", &[Scalar::Int(8)]), Some(Scalar::Int(4)));
        assert_eq!(
            sc.fold(&Scalar::Str("abc".into()), "rjust", &[Scalar::Int(5), Scalar::Str(".".into())]),
            Some(Scalar::Str("..abc".into()))
        );
        // A raising call declines (never crashes the worker).
        assert_eq!(sc.fold(&Scalar::Int(1), "to_s", &[Scalar::Int(99)]), None);
        // The worker is still alive after a decline.
        assert!(sc.ping());
    }
}
