//! CLI (ADR-0015): present the reference's full command surface; commands not
//! yet implemented in this phase report a clear message with a distinct exit
//! code, never a cryptic "unknown command".
//!
//! The tracer-bullet slice wires `rigor check <file...>` end to end: read ->
//! parse (ADR-0003) -> lower (ADR-0012) -> run rules (ADR-0005/0030) -> print.
use std::process::ExitCode;

use rigor_index::CoreIndex;
use rigor_parse::{lower, parse};
use rigor_rules::{analyze, Diagnostic};
use rigor_types::Interner;

/// The reference's full subcommand surface (ADR-0015).
const COMMANDS: &[&str] = &[
    "check", "annotate", "type-of", "trace", "type-scan", "explain", "diff",
    "sig-gen", "baseline", "triage", "coverage", "plugins", "plugin", "lsp",
    "mcp", "skill", "docs", "init",
];

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("check") => cmd_check(&args[1..]),
        Some(cmd) if COMMANDS.contains(&cmd) => {
            eprintln!("rigor-rs: `{cmd}` is recognized but not yet implemented in this phase");
            ExitCode::from(2)
        }
        Some(other) => {
            eprintln!("rigor-rs: unknown command `{other}`");
            ExitCode::from(2)
        }
        None => {
            eprintln!("rigor-rs (pre-alpha). usage: rigor <command>");
            eprintln!("commands: {}", COMMANDS.join(", "));
            ExitCode::from(2)
        }
    }
}

/// `rigor check [--format text|json] <file...>` — analyze each file and print
/// its diagnostics. Exit 1 if any diagnostic is found, 0 if none, 64 on a usage
/// error (ADR-0030 exit codes).
fn cmd_check(args: &[String]) -> ExitCode {
    let mut format = OutputFormat::Text;
    let mut files: Vec<&str> = Vec::new();

    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--format" => match it.next().map(String::as_str) {
                Some("text") => format = OutputFormat::Text,
                Some("json") => format = OutputFormat::Json,
                other => {
                    eprintln!("rigor check: --format expects `text` or `json`, got {other:?}");
                    return ExitCode::from(64);
                }
            },
            other => files.push(other),
        }
    }

    if files.is_empty() {
        eprintln!("rigor check: expected at least one file");
        return ExitCode::from(64);
    }

    let index = CoreIndex::new();
    let mut findings: Vec<(String, String, Diagnostic)> = Vec::new();
    let mut had_io_error = false;

    for path in &files {
        let source = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("rigor check: cannot read {path}: {e}");
                had_io_error = true;
                continue;
            }
        };
        let result = parse(source.as_bytes());
        let ast = lower(&result);
        let mut interner = Interner::new();
        for diag in analyze(&ast, &mut interner, &index) {
            findings.push((path.to_string(), source.clone(), diag));
        }
    }

    match format {
        OutputFormat::Text => print_text(&findings),
        OutputFormat::Json => print_json(&findings),
    }

    if had_io_error {
        ExitCode::from(1)
    } else if findings.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

/// Human format: `path:line:col: error: <message>` (1-based line/col).
fn print_text(findings: &[(String, String, Diagnostic)]) {
    for (path, source, diag) in findings {
        let (line, col) = line_col(source, diag.start_offset);
        println!("{path}:{line}:{col}: error: {}", diag.message);
    }
}

/// JSON format: a flat array of `{path,line,column,rule,message}` objects.
/// Hand-rolled (no serde dependency) — the field set is small and fixed.
fn print_json(findings: &[(String, String, Diagnostic)]) {
    let mut buf = String::from("[");
    for (idx, (path, source, diag)) in findings.iter().enumerate() {
        if idx > 0 {
            buf.push(',');
        }
        let (line, col) = line_col(source, diag.start_offset);
        buf.push_str(&format!(
            "{{\"path\":{},\"line\":{},\"column\":{},\"rule\":{},\"message\":{}}}",
            json_string(path),
            line,
            col,
            json_string(diag.rule_id),
            json_string(&diag.message),
        ));
    }
    buf.push(']');
    println!("{buf}");
}

/// Compute 1-based (line, column) from a UTF-8 byte offset into `source`.
/// Columns are counted in Unicode scalar values for the line, which matches the
/// reference's column semantics closely enough for the tracer bullet.
// TODO(spec): align column counting with the reference's exact unit (it adds 1
// to Prism's 0-based byte/char column) once a parity fixture pins it.
fn line_col(source: &str, byte_offset: usize) -> (usize, usize) {
    let clamped = byte_offset.min(source.len());
    let mut line = 1usize;
    let mut line_start = 0usize;
    for (i, b) in source.as_bytes().iter().enumerate() {
        if i >= clamped {
            break;
        }
        if *b == b'\n' {
            line += 1;
            line_start = i + 1;
        }
    }
    // Column = char count between the line start and the offset, plus 1.
    let col = source[line_start..clamped].chars().count() + 1;
    (line, col)
}

/// Minimal JSON string escaper for the small, ASCII-ish strings we emit.
fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Output format for `rigor check` (ADR-0014; text default, json nice-to-have).
enum OutputFormat {
    Text,
    Json,
}
