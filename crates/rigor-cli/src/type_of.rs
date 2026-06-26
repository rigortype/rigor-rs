//! `rigor type-of FILE:LINE:COL` (ADR-0015) — the expression-typer probe.
//!
//! A thin probe over the same parse + [`Typer`] path `check` runs: it locates the
//! deepest lowered node whose byte span contains the `(line, column)` position,
//! types it with [`Typer::type_of`] against the file's top-level env, and prints
//! the inferred type. The command exposes existing capability (the typer) and
//! does NOT touch inference.
//!
//! ## Parity note — rendering
//!
//! The reference renders the node as its `Prism::<Node>Class` and the type via
//! `Type#describe` + `erase_to_rbs` (value-pinned: `"hello"`, `[1, 2, 3]`). rigor-rs
//! walks an OWNED [`rigor_parse::Node`] arena (ADR-0012) with its own type
//! display, so the `node:` line names the rigor-rs node variant and the `type:`
//! line uses rigor-rs's [`render_type`] — the SAME spelling `check`'s
//! `receiver_type` field uses (a Constant renders its value, e.g. `"hello"`; a
//! nominal renders its class name, e.g. `String`). The layout (`file:line:col`,
//! `node:`, `type:` for text; the same keys for json) mirrors the reference; the
//! `erased`/`fallbacks`/`--trace` fields the reference carries are rigor-rs-specific
//! follow-ons (no `erase_to_rbs` / FallbackTracer in this port yet) and are omitted.

use std::path::Path;
use std::process::ExitCode;

use rigor_index::CoreIndex;
use rigor_infer::{SourceIndex, Typer};
use rigor_parse::{lower, parse, LoweredAst, Node, NodeId};
use rigor_types::{Interner, Scalar, Type, TypeId};

/// A located, typed result, ready to render (mirrors the reference's `Result`).
struct Probe {
    file: String,
    line: usize,
    column: usize,
    node_kind: &'static str,
    type_render: String,
}

/// `rigor type-of [--format text|json] FILE:LINE:COL` (or `FILE LINE COL`).
/// Exit 0 on success, 1 on a missing file / no-expression-at-position, 64 on a
/// usage error (bad args or an out-of-range position), mirroring the reference's
/// exit codes.
pub fn cmd_type_of(args: &[String]) -> ExitCode {
    let mut format = "text";
    let mut positional: Vec<&str> = Vec::new();

    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--format" => match it.next().map(String::as_str) {
                Some(f @ ("text" | "json")) => format = f,
                other => {
                    eprintln!("rigor type-of: --format expects `text` or `json`, got {other:?}");
                    return ExitCode::from(64);
                }
            },
            other => positional.push(other),
        }
    }

    let target = match parse_position(&positional) {
        Some(t) => t,
        None => return ExitCode::from(64),
    };
    let (file, line, column) = target;

    // The file must exist on disk (no editor-mode buffer binding in this port).
    if !Path::new(file).is_file() {
        eprintln!("type-of: file not found: {file}");
        return ExitCode::from(1);
    }

    let source = match std::fs::read_to_string(file) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("type-of: cannot read {file}: {e}");
            return ExitCode::from(1);
        }
    };

    // Resolve the (line, column) position to a byte offset into the source. An
    // out-of-range line/column is a usage error (exit 64), like the reference's
    // `NodeLocator::OutOfRangeError` path.
    let offset = match position_to_offset(&source, line, column) {
        Ok(off) => off,
        Err(msg) => {
            eprintln!("type-of: {msg}");
            return ExitCode::from(64);
        }
    };

    // Parse + lower exactly as `check` does (the same project path).
    let bytes = source.as_bytes();
    let ast = lower(&parse(bytes));

    // Locate the deepest node whose span contains the offset.
    let Some(node_id) = locate_node(&ast, offset) else {
        eprintln!("type-of: no expression found at {file}:{line}:{column}");
        return ExitCode::from(1);
    };

    // Build the same typer + top-level env `check` uses. A single-file source
    // index is enough for a one-file probe (the project-wide cross-file index is
    // a `check`-only concern).
    let index = CoreIndex::new();
    let source_index = SourceIndex::build(&ast, &index);
    let typer = Typer::with_source(&index, &source_index);
    let mut interner = Interner::new();
    let env = typer.build_toplevel_env(&ast, &mut interner);

    let ty = typer.type_of(&ast, node_id, &env, &mut interner);
    let probe = Probe {
        file: file.to_string(),
        line,
        column,
        node_kind: node_kind(ast.get(node_id)),
        type_render: render_type(&interner, &index, ty),
    };

    match format {
        "json" => render_json(&probe),
        _ => render_text(&probe),
    }
    ExitCode::SUCCESS
}

// ---------------------------------------------------------------------------
// Argument parsing
// ---------------------------------------------------------------------------

/// Parse the position argument: either one `FILE:LINE:COL` token or three
/// `FILE LINE COL` tokens (reference `parse_position_argument`). Returns
/// `(file, line, column)` or `None` after printing a usage error.
fn parse_position<'a>(argv: &[&'a str]) -> Option<(&'a str, usize, usize)> {
    match argv.len() {
        1 => parse_colon_form(argv[0]),
        3 => decode_position(argv[0], argv[1], argv[2]),
        _ => {
            eprintln!("type-of: expected FILE:LINE:COL or FILE LINE COL");
            eprintln!("Usage: rigor type-of [options] FILE:LINE:COL");
            None
        }
    }
}

/// Split a `FILE:LINE:COL` token, taking the last two colon-separated parts as
/// line/column so a path containing colons still parses (reference
/// `parse_colon_form`).
fn parse_colon_form(arg: &str) -> Option<(&str, usize, usize)> {
    let parts: Vec<&str> = arg.split(':').collect();
    if parts.len() < 3 {
        eprintln!("type-of: expected FILE:LINE:COL, got {arg:?}");
        eprintln!("Usage: rigor type-of [options] FILE:LINE:COL");
        return None;
    }
    let column = parts[parts.len() - 1];
    let line = parts[parts.len() - 2];
    let file_end = arg.len() - column.len() - line.len() - 2;
    let file = &arg[..file_end];
    decode_position(file, line, column)
}

/// Parse the line/column ints (reference `decode_position`).
fn decode_position<'a>(file: &'a str, line: &str, column: &str) -> Option<(&'a str, usize, usize)> {
    match (line.parse::<usize>(), column.parse::<usize>()) {
        (Ok(l), Ok(c)) => Some((file, l, c)),
        _ => {
            eprintln!("type-of: line and column must be integers");
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Position → byte offset
// ---------------------------------------------------------------------------

/// Resolve a 1-based `(line, column)` to a 0-based byte offset into `source`.
/// The column is 1-based and counted in Unicode scalar values, the inverse of
/// the presenter's [`crate::line_col`]-style mapping. An out-of-range line or
/// column yields an `Err` with the reference's wording.
fn position_to_offset(source: &str, line: usize, column: usize) -> Result<usize, String> {
    if line == 0 || column == 0 {
        return Err("line and column are 1-based".to_string());
    }
    // Walk to the start of the target line.
    let mut current_line = 1usize;
    let mut line_start = 0usize;
    let bytes = source.as_bytes();
    let mut i = 0usize;
    while current_line < line {
        match bytes.get(i) {
            Some(b'\n') => {
                current_line += 1;
                line_start = i + 1;
                i += 1;
            }
            Some(_) => i += 1,
            None => return Err(format!("line {line} is past the end of the source buffer")),
        }
    }
    // The line's text spans `line_start` up to the next newline (or EOF).
    let line_end = source[line_start..]
        .find('\n')
        .map(|n| line_start + n)
        .unwrap_or(source.len());
    let line_text = &source[line_start..line_end];

    // Advance `column - 1` Unicode scalars into the line.
    let mut col_offset = 0usize;
    for (idx, _) in line_text.char_indices() {
        if col_offset == column - 1 {
            return Ok(line_start + idx);
        }
        col_offset += 1;
    }
    // Column at the line's end-of-text (one past the last char) is valid and
    // points at the newline / EOF byte.
    if column - 1 == col_offset {
        return Ok(line_end);
    }
    Err(format!(
        "column {column} is past the end of line {line}"
    ))
}

// ---------------------------------------------------------------------------
// Node-at-position lookup
// ---------------------------------------------------------------------------

/// The deepest (smallest-span) node whose byte span contains `offset`. Mirrors
/// the reference's `NodeLocator.at_position` "deepest expression at a position":
/// among all nodes covering the offset, the one with the narrowest span wins;
/// ties break toward the later (more specific, deeper-lowered) node id.
///
/// The owned arena carries a [`rigor_parse::Span`] (half-open `[start, end)`) on
/// every node, so this is a single linear scan. The root `Program`/`Statements`
/// wrappers always cover the offset but lose the smallest-span contest to any
/// real expression beneath them.
fn locate_node(ast: &LoweredAst, offset: usize) -> Option<NodeId> {
    let mut best: Option<(NodeId, usize)> = None; // (id, span width)
    for (id, node) in ast.iter() {
        let (start, end) = node.span();
        if start <= offset && offset < end {
            let width = end - start;
            match best {
                // Strictly-narrower span wins; on an equal width prefer the later
                // id (deeper / more specific node lowered after its container).
                Some((_, best_w)) if width <= best_w => {
                    best = Some((id, width));
                }
                None => best = Some((id, width)),
                _ => {}
            }
        }
    }
    best.map(|(id, _)| id)
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// A rigor-rs-native node-variant label for the `node:` line. The reference
/// prints the Prism node class (`Prism::StringNode`); rigor-rs walks an owned
/// arena, so we name the owned variant.
fn node_kind(node: &Node) -> &'static str {
    match node {
        Node::Program { .. } => "Program",
        Node::Statements { .. } => "Statements",
        Node::LocalVariableWrite { .. } => "LocalVariableWrite",
        Node::LocalVariableOpWrite { .. } => "LocalVariableOpWrite",
        Node::LocalVariableRead { .. } => "LocalVariableRead",
        Node::StringLit { .. } => "StringLit",
        Node::InterpolatedString { .. } => "InterpolatedString",
        Node::IntegerLit { .. } => "IntegerLit",
        Node::FloatLit { .. } => "FloatLit",
        Node::SymbolLit { .. } => "SymbolLit",
        Node::NilLit { .. } => "NilLit",
        Node::TrueLit { .. } => "TrueLit",
        Node::FalseLit { .. } => "FalseLit",
        Node::Call { .. } => "Call",
        Node::Definition { .. } => "Definition",
        Node::ClassDef { .. } => "ClassDef",
        Node::ModuleDef { .. } => "ModuleDef",
        Node::If { .. } => "If",
        Node::Case { .. } => "Case",
        Node::Loop { .. } => "Loop",
        Node::BeginRescue { .. } => "BeginRescue",
        Node::Logical { .. } => "Logical",
        Node::ArrayLit { .. } => "ArrayLit",
        Node::HashLit { .. } => "HashLit",
        Node::Range { .. } => "Range",
        Node::VariableRead { .. } => "VariableRead",
        Node::VariableWrite { .. } => "VariableWrite",
        Node::ConstantRead { .. } => "ConstantRead",
        Node::ConstantWrite { .. } => "ConstantWrite",
        Node::SelfExpr { .. } => "SelfExpr",
        Node::Other { .. } => "Other",
    }
}

/// Render a type for the `type:` line, using the SAME spelling `check`'s
/// `receiver_type` field uses: a `Constant` renders its value (`"hello"`, `3`,
/// `:foo`, `nil`); a carrier with a known class name renders that name
/// (`String`, `singleton(Time)`); anything else falls back to rigor-rs's
/// [`rigor_types::describe`] (`Dynamic[top]`, unions, …).
fn render_type(interner: &Interner, index: &CoreIndex, ty: TypeId) -> String {
    if let Type::Constant(scalar) = interner.get(ty) {
        return render_scalar(scalar);
    }
    if let Some(name) = index.class_name_of(interner, ty) {
        return name.to_string();
    }
    rigor_types::describe(interner, ty)
}

/// Render a scalar literal as it appears in `check`'s output: strings quoted
/// (`"hello"`), symbols colon-prefixed (`:foo`), everything else by its natural
/// spelling. Mirrors `rigor_rules`'s `render_scalar`.
fn render_scalar(scalar: &Scalar) -> String {
    match scalar {
        Scalar::Str(s) => format!("{s:?}"),
        Scalar::Sym(s) => format!(":{s}"),
        Scalar::Int(n) => n.to_string(),
        Scalar::Float(f) => f.to_string(),
        Scalar::Bool(b) => b.to_string(),
        Scalar::Nil => "nil".to_string(),
    }
}

/// Text rendering (mirrors the reference's `render_text` layout).
fn render_text(probe: &Probe) {
    println!("{}:{}:{}", probe.file, probe.line, probe.column);
    println!("node:    {}", probe.node_kind);
    println!("type:    {}", probe.type_render);
}

/// JSON rendering (the reference's `render_json` key set, minus `erased`).
fn render_json(probe: &Probe) {
    use serde_json::json;
    let payload = json!({
        "file": probe.file,
        "line": probe.line,
        "column": probe.column,
        "node": probe.node_kind,
        "type": probe.type_render,
    });
    println!("{}", serde_json::to_string_pretty(&payload).unwrap());
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Type the node located at a 1-based (line, column) in `src`, returning
    /// `(node_kind, type_render)` — the two fields `type-of` prints.
    fn probe(src: &str, line: usize, column: usize) -> (&'static str, String) {
        let offset = position_to_offset(src, line, column).expect("in range");
        let ast = lower(&parse(src.as_bytes()));
        let id = locate_node(&ast, offset).expect("a node at position");
        let index = CoreIndex::new();
        let source_index = SourceIndex::build(&ast, &index);
        let typer = Typer::with_source(&index, &source_index);
        let mut interner = Interner::new();
        let env = typer.build_toplevel_env(&ast, &mut interner);
        let ty = typer.type_of(&ast, id, &env, &mut interner);
        (node_kind(ast.get(id)), render_type(&interner, &index, ty))
    }

    #[test]
    fn types_a_string_literal() {
        // `x = "hello"` — column 5 lands on the string literal.
        let (kind, ty) = probe("x = \"hello\"\n", 1, 5);
        assert_eq!(kind, "StringLit");
        assert_eq!(ty, "\"hello\"");
    }

    #[test]
    fn types_an_integer_literal() {
        let (kind, ty) = probe("n = 42\n", 1, 5);
        assert_eq!(kind, "IntegerLit");
        assert_eq!(ty, "42");
    }

    #[test]
    fn types_a_local_read_from_env() {
        // `s = "hi"; s.upcase` — `s` on line 2 col 1 reads the env binding.
        let (kind, ty) = probe("s = \"hi\"\ns.upcase\n", 2, 1);
        assert_eq!(kind, "LocalVariableRead");
        assert_eq!(ty, "\"hi\"");
    }

    #[test]
    fn types_a_chained_call_result() {
        // `s = "hi"; s.upcase` — column 3 on line 2 lands on `upcase`, whose
        // deepest covering node is the Call. rigor-rs constant-folds the chained
        // call, so the result is the value-pinned `"HI"` (matching the reference
        // oracle, which prints `"HELLO"` for `x.upcase` on `x = "hello"`).
        let (kind, ty) = probe("s = \"hi\"\ns.upcase\n", 2, 3);
        assert_eq!(kind, "Call");
        assert_eq!(ty, "\"HI\"");
    }

    #[test]
    fn out_of_range_line_is_an_error() {
        let err = position_to_offset("x = 1\n", 99, 1).unwrap_err();
        assert!(err.contains("past the end of the source buffer"), "{err}");
    }

    #[test]
    fn colon_form_parses_path_line_col() {
        let (f, l, c) = parse_colon_form("a/b.rb:12:7").unwrap();
        assert_eq!((f, l, c), ("a/b.rb", 12, 7));
    }

    #[test]
    fn colon_form_keeps_colons_in_path() {
        // A path with colons keeps everything before the last two parts.
        let (f, l, c) = parse_colon_form("C:/x.rb:3:1").unwrap();
        assert_eq!((f, l, c), ("C:/x.rb", 3, 1));
    }
}
