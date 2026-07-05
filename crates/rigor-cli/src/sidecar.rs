//! ADR-0008 / ADR-0036 — the Ruby sidecar client (Slice 1: spawn + handshake +
//! liveness probe).
//!
//! The sidecar is the project's Ruby running [`SIDECAR_RB`] as a request loop
//! (newline-delimited JSON over stdin/stdout). This module owns spawning it and
//! the **availability probe** ADR-0036 gates the coverage posture on: spawn the
//! ruby, read its handshake line, exchange one `ping`, and report whether the
//! round trip succeeded. A present-but-broken Ruby (version skew, missing stdlib)
//! fails the probe and counts as unavailable — exactly the "handshake, not a bare
//! `ruby` on PATH" rule.
//!
//! Slice 1 exposes the probe only through `rigor doctor` (a diagnostic command
//! where spawning ruby to report reachability is appropriate). Routing constant
//! folding through a *persistent* sidecar, and the exit-69 hard error `require`
//! grows, land in later slices — see the module note in `ruby_mode`.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use crate::ruby_mode::RubyMode;

/// The embedded sidecar script — shipped in the binary (`include_str!`) so the
/// single-artifact distribution is preserved (no external file to locate).
pub const SIDECAR_RB: &str = include_str!("sidecar.rb");

/// The protocol version the client understands; must match the sidecar's
/// `rigor_sidecar` handshake field.
const PROTOCOL_VERSION: u64 = 1;

/// How long to wait for the spawn + handshake + ping round trip before declaring
/// the sidecar unavailable, so a hung or wrong-version ruby never blocks the run.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// A successful probe result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Handshake {
    /// The sidecar ruby's `RUBY_VERSION`.
    pub ruby_version: String,
}

/// Why a probe decided the sidecar is unavailable. All map to "run the sound
/// subset" (or, once `require` grows teeth, the exit-69 hard error).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeError {
    /// The ruby binary could not be spawned (not found / not executable).
    Spawn(String),
    /// No usable handshake line (EOF, non-JSON, wrong protocol version).
    Handshake,
    /// The handshake succeeded but the `ping` round trip did not.
    Ping,
    /// The round trip did not complete within [`PROBE_TIMEOUT`].
    Timeout,
}

impl std::fmt::Display for ProbeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProbeError::Spawn(e) => write!(f, "could not spawn ruby ({e})"),
            ProbeError::Handshake => f.write_str("no usable sidecar handshake"),
            ProbeError::Ping => f.write_str("sidecar ping round trip failed"),
            ProbeError::Timeout => f.write_str("sidecar probe timed out"),
        }
    }
}

/// The ruby binary a mode would spawn, or `None` when the mode opts out of the
/// sidecar (`off`). `Require`/`Auto` use `ruby` on PATH (project-Ruby / bundler
/// detection is a later slice); `Path(p)` names the binary explicitly.
#[must_use]
pub fn ruby_bin_for(mode: &RubyMode) -> Option<String> {
    match mode {
        RubyMode::Off => None,
        RubyMode::Path(p) => Some(p.clone()),
        RubyMode::Require | RubyMode::Auto => Some("ruby".to_string()),
    }
}

/// Spawn `ruby_bin` running the sidecar, confirm the handshake, and exchange one
/// `ping`. Returns the [`Handshake`] on success. Timeout-guarded and side-effect
/// free (the worker is shut down before returning), so it is safe to call as an
/// availability check. The heavy lifting runs on a worker thread so a hung child
/// cannot block past [`PROBE_TIMEOUT`]; the child is killed on every path.
pub fn probe(ruby_bin: &str) -> Result<Handshake, ProbeError> {
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

    // The handshake+ping exchange runs on a worker thread; the main thread waits
    // with a timeout and kills the child regardless of outcome.
    std::thread::spawn(move || {
        let _ = tx.send(handshake_and_ping(stdin, stdout));
    });

    let result = match rx.recv_timeout(PROBE_TIMEOUT) {
        Ok(r) => r,
        Err(_) => Err(ProbeError::Timeout),
    };
    let _ = child.kill();
    let _ = child.wait();
    result
}

/// The blocking half of [`probe`], run on a worker thread: read the handshake
/// line, verify the protocol version, send a `ping`, and read the reply.
fn handshake_and_ping(
    stdin: Option<std::process::ChildStdin>,
    stdout: Option<std::process::ChildStdout>,
) -> Result<Handshake, ProbeError> {
    let (mut stdin, stdout) = match (stdin, stdout) {
        (Some(i), Some(o)) => (i, o),
        _ => return Err(ProbeError::Handshake),
    };
    let mut reader = BufReader::new(stdout);

    // 1) Handshake line.
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

    // 2) Ping round trip — proves the request loop is live, not just the banner.
    stdin
        .write_all(b"{\"op\":\"ping\"}\n")
        .map_err(|_| ProbeError::Ping)?;
    stdin.flush().map_err(|_| ProbeError::Ping)?;
    let mut reply = String::new();
    if reader.read_line(&mut reply).unwrap_or(0) == 0 {
        return Err(ProbeError::Ping);
    }
    let reply: serde_json::Value =
        serde_json::from_str(reply.trim()).map_err(|_| ProbeError::Ping)?;
    if reply.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
        return Err(ProbeError::Ping);
    }

    // 3) Ask the worker to exit (best effort; the caller also kills it).
    let _ = stdin.write_all(b"{\"op\":\"shutdown\"}\n");
    let _ = stdin.flush();

    Ok(Handshake { ruby_version })
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
        assert!(!hs.ruby_version.is_empty());
        // The reported version looks like a Ruby version (starts with a digit).
        assert!(hs.ruby_version.chars().next().is_some_and(|c| c.is_ascii_digit()));
    }
}
