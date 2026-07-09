//! `rigor annotate FILE` — a port of the reference's `AnnotateCommand` +
//! `LineTypeCollector` (`lib/rigor/cli/annotate_command.rb`).
//!
//! For every source line the command finds the expression the line evaluates to
//! and appends a `#=> <type>` comment (the xmpfilter / seeing_is_believing
//! convention), using the shared `describe_named` display layer for the type
//! string. `--format json` emits the `{ line => type }` map instead.
//!
//! ## Per-line selection (reference `LineTypeCollector`)
//!
//! 1. Every STATEMENT (a direct child of a `Statements`/`Program` body) sets
//!    `by_line[end_line] = type`. rigor-rs's arena is lowered bottom-up, so a
//!    parent statement has a higher `NodeId` than the nested statements it
//!    contains and than its earlier siblings; processing statements in ascending
//!    `NodeId` order therefore reproduces the reference's post-order "outermost /
//!    last sibling closing a line wins" overwrite.
//! 2. A `def` statement types to its method-name symbol (`:greet`), and its
//!    HEADER line is overridden with the method's inferred return type (or the
//!    annotation is dropped when the return cannot be inferred).
//! 3. A line no statement closes (an `if`/block header) falls back to the widest
//!    expression ending there.
//!
//! ## Scope / deferrals
//!
//! - Types are inferred against the TOP-LEVEL env (rigor-rs has no per-scope
//!   `ScopeIndexer`), so a top-level script annotates exactly like the reference;
//!   a method-body local that depends on a param/ivar types `Dynamic[top]` (as it
//!   does in the reference), but a def-LOCAL literal binding (`z = 5; z` inside a
//!   method) types `Dynamic[top]` here where the reference's scope would pin `5`.
//! - Colour output (the reference's `bat` / IRB-style highlighting) is deferred:
//!   the annotated source is printed plain. `--color` / `--no-color` / `--bat` /
//!   `--no-bat` are accepted for CLI compatibility and ignored.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::ExitCode;

use rigor_index::CoreIndex;
use rigor_infer::{SourceIndex, TypeEnv, Typer};
use rigor_parse::{lower, parse, LoweredAst, Node, NodeId};
use rigor_types::Interner;

const USAGE: &str = "Usage: rigor annotate [options] FILE";

/// `rigor annotate [--format text|json] [--[no-]color] [--[no-]bat] FILE`.
pub fn cmd_annotate(args: &[String]) -> ExitCode {
    let mut format = "text";
    let mut file: Option<&str> = None;

    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--format" => match it.next().map(String::as_str) {
                Some(f @ ("text" | "json")) => format = f,
                other => {
                    eprintln!("rigor annotate: --format expects `text` or `json`, got {other:?}");
                    return ExitCode::from(64);
                }
            },
            other if other.starts_with("--format=") => match &other["--format=".len()..] {
                f @ ("text" | "json") => format = f,
                v => {
                    eprintln!("rigor annotate: unsupported format: {v}");
                    return ExitCode::from(64);
                }
            },
            // Colour flags accepted for CLI compatibility; output is plain.
            "--color" | "--no-color" | "--bat" | "--no-bat" => {}
            other if other.starts_with('-') => {
                eprintln!("rigor annotate: unknown option {other:?}");
                return ExitCode::from(64);
            }
            other => file = Some(other),
        }
    }

    let Some(file) = file else {
        eprintln!("{USAGE}");
        return ExitCode::from(64);
    };
    if !Path::new(file).is_file() {
        eprintln!("annotate: file not found: {file}");
        return ExitCode::from(1);
    }
    let source = match std::fs::read_to_string(file) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("annotate: cannot read {file}: {e}");
            return ExitCode::from(1);
        }
    };

    let ast = lower(&parse(source.as_bytes()));
    let index = CoreIndex::new();
    let source_index = SourceIndex::build(&ast, &index);
    let typer = Typer::with_source(&index, &source_index);
    let mut interner = Interner::new();
    let env = typer.build_toplevel_env(&ast, &mut interner);

    let line_types =
        collect_line_types(&ast, &typer, &source_index, &index, &env, &mut interner, &source);

    match format {
        "json" => emit_json(&line_types),
        _ => print!("{}", annotate_text(&source, &line_types)),
    }
    ExitCode::SUCCESS
}

/// The 1-based line a byte offset sits on.
fn line_of(source: &str, offset: usize) -> usize {
    crate::line_col(source, offset).0
}

/// The 1-based line a node's span ENDS on (the last character, since the span's
/// end offset is exclusive).
fn end_line(source: &str, node: &Node) -> usize {
    let (_start, end) = node.span();
    line_of(source, end.saturating_sub(1))
}

/// Resolve every source line to the type of the expression it evaluates to,
/// keyed 1-based. Ported from the reference `LineTypeCollector#collect`.
fn collect_line_types(
    ast: &LoweredAst,
    typer: &Typer,
    source_index: &SourceIndex,
    index: &CoreIndex,
    env: &TypeEnv,
    interner: &mut Interner,
    source: &str,
) -> BTreeMap<usize, String> {
    let describe = |interner: &Interner, ty| crate::type_display::describe(interner, index, source_index, ty);
    // The type string a node evaluates to: a `def` → its method-name symbol
    // (`:greet`); an assignment → its RHS value (the reference `StatementEvaluator`
    // evaluates a write to its value, whereas the expression typer leaves the
    // write node `Dynamic[top]`); anything else → its own type.
    let type_at = |id: NodeId, interner: &mut Interner| -> String {
        if let Node::Definition { name: Some(name), .. } = ast.get(id) {
            return format!(":{name}");
        }
        let target = match ast.get(id) {
            Node::LocalVariableWrite { value, .. }
            | Node::LocalVariableOpWrite { value, .. }
            | Node::VariableWrite { value, .. }
            | Node::ConstantWrite { value, .. } => *value,
            _ => id,
        };
        let ty = typer.type_of(ast, target, env, interner);
        describe(interner, ty)
    };
    let mut by_line: BTreeMap<usize, String> = BTreeMap::new();

    // (1) Statement lines. Collect every statement id (a child of any
    // statement-list carrier — a `Statements`/`Program` body or a branch/loop/
    // def/class body, which rigor-rs stores as flat `Vec<NodeId>` fields rather
    // than wrapping in `Statements`). Process in ascending NodeId order so the
    // outermost / last statement closing a line wins (post-order overwrite).
    let mut statement_ids: Vec<NodeId> = Vec::new();
    for (_id, node) in ast.iter() {
        push_statement_children(node, &mut statement_ids);
    }
    statement_ids.sort_unstable();
    statement_ids.dedup();
    for id in &statement_ids {
        let line = end_line(source, ast.get(*id));
        let s = type_at(*id, interner);
        by_line.insert(line, s);
    }

    // (2) Widest-expression fallback for lines no statement closes (an `if` /
    // block header reports its condition). Track the max-span expression ending
    // on each line, then fill only lines not already covered.
    let mut widest: BTreeMap<usize, (usize, NodeId)> = BTreeMap::new();
    for (id, node) in ast.iter() {
        if matches!(node, Node::Program { .. } | Node::Statements { .. }) {
            continue;
        }
        let (start, end) = node.span();
        let line = end_line(source, node);
        let span = end.saturating_sub(start);
        match widest.get(&line) {
            Some((best, _)) if *best >= span => {}
            _ => {
                widest.insert(line, (span, id));
            }
        }
    }
    for (line, (_span, id)) in widest {
        if by_line.contains_key(&line) {
            continue;
        }
        let s = type_at(id, interner);
        by_line.insert(line, s);
    }

    // (3) def-header override: the header line (where `def` sits) reports the
    // method's inferred RETURN type, not the param list's `Dynamic[top]`; when
    // the return cannot be inferred the annotation is dropped.
    for (id, node) in ast.iter() {
        if let Node::Definition { name: Some(_), has_explicit_return, body, .. } = node {
            let start_line = line_of(source, ast.get(id).span().0);
            match def_return_type(ast, typer, body, *has_explicit_return, env, interner, &describe) {
                Some(t) => {
                    by_line.insert(start_line, t);
                }
                None => {
                    by_line.remove(&start_line);
                }
            }
        }
    }

    by_line
}

/// Extend `out` with the statement children of `node` — the elements of every
/// statement-list field it carries. rigor-rs stores a branch / loop / def / class
/// body as a flat `Vec<NodeId>` on the carrier node (not wrapped in a
/// `Statements`), so each such field is a source of statements. A carrier not
/// listed here is covered by the widest-expression fallback instead.
fn push_statement_children(node: &Node, out: &mut Vec<NodeId>) {
    match node {
        Node::Program { body, .. }
        | Node::Statements { body, .. }
        | Node::Definition { body, .. }
        | Node::ClassDef { body, .. }
        | Node::ModuleDef { body, .. }
        | Node::Loop { body, .. }
        | Node::BeginRescue { body, .. }
        | Node::Call { block_body: body, .. } => out.extend(body.iter().copied()),
        Node::If { then_body, else_body, .. } => {
            out.extend(then_body.iter().copied());
            out.extend(else_body.iter().copied());
        }
        _ => {}
    }
}

/// A method's inferred return type for the def-header annotation, or `None` to
/// drop it. Mirrors the reference `DefReturnTyper`: `nil` only when the body is
/// empty; otherwise the last statement's type (which may be `Dynamic[top]` — a
/// param/ivar-dependent or branch tail — and is shown as such).
fn def_return_type(
    ast: &LoweredAst,
    typer: &Typer,
    body: &[NodeId],
    _has_explicit_return: bool,
    env: &TypeEnv,
    interner: &mut Interner,
    describe: &dyn Fn(&Interner, rigor_types::TypeId) -> String,
) -> Option<String> {
    let &tail = body.last()?;
    // An assignment tail evaluates to its RHS value (StatementEvaluator).
    let target = match ast.get(tail) {
        Node::LocalVariableWrite { value, .. }
        | Node::LocalVariableOpWrite { value, .. }
        | Node::VariableWrite { value, .. }
        | Node::ConstantWrite { value, .. } => *value,
        _ => tail,
    };
    let ty = typer.type_of(ast, target, env, interner);
    Some(describe(interner, ty))
}

/// The trailing `#=> …` annotation (with a leading space), stripped before
/// re-annotating so a re-run is idempotent (reference `ANNOTATION_PATTERN`).
fn strip_annotation(line: &str) -> &str {
    match line.find("#=>") {
        // Require whitespace before `#=>` so a `#=>` inside a string literal at
        // column 0 isn't stripped mid-expression.
        Some(idx) if idx > 0 && line.as_bytes()[idx - 1].is_ascii_whitespace() => {
            line[..idx].trim_end()
        }
        _ => line.trim_end_matches(['\n', '\r']),
    }
}

/// Append ` #=> <type>` to each annotated line, aligning the comment column to
/// the widest annotated code line (reference `annotate` + `annotation_column`).
fn annotate_text(source: &str, line_types: &BTreeMap<usize, String>) -> String {
    let lines: Vec<&str> = source.split_inclusive('\n').collect();
    let column = lines
        .iter()
        .enumerate()
        .filter(|(i, _)| line_types.contains_key(&(i + 1)))
        .map(|(_, l)| strip_annotation(l).chars().count())
        .max()
        .unwrap_or(0);

    let mut out = String::new();
    for (i, line) in lines.iter().enumerate() {
        let eol = if line.ends_with('\n') { "\n" } else { "" };
        let code = strip_annotation(line);
        match line_types.get(&(i + 1)) {
            Some(ty) => {
                let pad = column.saturating_sub(code.chars().count());
                out.push_str(code);
                out.push_str(&" ".repeat(pad));
                out.push_str("  #=> ");
                out.push_str(ty);
                out.push_str(eol);
            }
            None => {
                out.push_str(code);
                out.push_str(eol);
            }
        }
    }
    out
}

/// `--format json` — the `{ "annotations": { "<line>": "<type>" } }` map
/// (reference `emit_json`), 1-based string line keys in ascending order.
fn emit_json(line_types: &BTreeMap<usize, String>) {
    println!("{}", annotations_json_string(line_types));
}

fn annotations_json_string(line_types: &BTreeMap<usize, String>) -> String {
    let entries: Vec<String> = line_types
        .iter()
        .map(|(line, ty)| format!("{}:{}", serde_json::to_string(&line.to_string()).unwrap(), serde_json::to_string(ty).unwrap()))
        .collect();
    format!("{{\"annotations\":{{{}}}}}", entries.join(","))
}

/// The `{ line => type }` annotations JSON for `source`, typed against `index`
/// (reused by the `annotate` MCP tool). Types are inferred against the top-level
/// env, exactly as `rigor annotate --format json`.
#[must_use]
pub fn annotations_json(index: &CoreIndex, source: &str) -> String {
    let ast = lower(&parse(source.as_bytes()));
    let source_index = SourceIndex::build(&ast, index);
    let typer = Typer::with_source(index, &source_index);
    let mut interner = Interner::new();
    let env = typer.build_toplevel_env(&ast, &mut interner);
    let line_types =
        collect_line_types(&ast, &typer, &source_index, index, &env, &mut interner, source);
    annotations_json_string(&line_types)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line_types(src: &str) -> BTreeMap<usize, String> {
        let ast = lower(&parse(src.as_bytes()));
        let index = CoreIndex::new();
        let source_index = SourceIndex::build(&ast, &index);
        let typer = Typer::with_source(&index, &source_index);
        let mut interner = Interner::new();
        let env = typer.build_toplevel_env(&ast, &mut interner);
        collect_line_types(&ast, &typer, &source_index, &index, &env, &mut interner, src)
    }

    fn annotate(src: &str) -> String {
        annotate_text(src, &line_types(src))
    }

    #[test]
    fn annotates_literal_assignments() {
        let lt = line_types("x = \"hi\"\ny = [1, 2]\nz = { a: 1 }\n");
        assert_eq!(lt.get(&1).map(String::as_str), Some("\"hi\""));
        assert_eq!(lt.get(&2).map(String::as_str), Some("[1, 2]"));
        assert_eq!(lt.get(&3).map(String::as_str), Some("{ a: 1 }"));
    }

    #[test]
    fn untyped_line_shows_dynamic() {
        let lt = line_types("u = foo\n");
        assert_eq!(lt.get(&1).map(String::as_str), Some("Dynamic[top]"));
    }

    #[test]
    fn def_symbol_and_return_override() {
        // Header → return type; body → the literal; `end` → the def-name symbol.
        let lt = line_types("def greet(n)\n  \"hi\"\nend\n");
        assert_eq!(lt.get(&1).map(String::as_str), Some("\"hi\""));
        assert_eq!(lt.get(&2).map(String::as_str), Some("\"hi\""));
        assert_eq!(lt.get(&3).map(String::as_str), Some(":greet"));
    }

    #[test]
    fn assignment_inside_if_body_is_typed() {
        // A branch-body assignment (rigor-rs stores it in `If.then_body`, not a
        // wrapped `Statements`) still gets its RHS type.
        let lt = line_types("if a\n  c = 3\nend\n");
        assert_eq!(lt.get(&2).map(String::as_str), Some("3"));
    }

    #[test]
    fn idempotent_reannotation() {
        let once = annotate("x = 5\n");
        // Feeding annotated source back in strips the old `#=>` first.
        let twice = annotate(&once);
        assert_eq!(once, twice);
    }
}
