//! Argument-dependent narrowing for catalogue rows — a port of upstream's
//! `lib/rigor/effects/narrowing.rb` (ADR-0043 slice 2).
//!
//! A row's `narrow:` names one of these handlers, and `core.yml`'s `why:` lines
//! say why each row wants one. A handler reads **the call's own argument
//! literals and nothing else**: the effect scan is observational, and a
//! narrowing that depended on inference would make the catalogue's answer a
//! function of analysis quality rather than of the source in front of it.
//!
//! # Where the port INVERTS upstream's contract
//!
//! Upstream's handlers are total and answer an **upper bound** — "when the
//! literal does not settle the question it returns the row's parent label"
//! (`narrowing.rb:19-21`). That is sound for upstream and an **over-claim
//! here**: ADR-0043 § 2 grades the proven lane as a raw STRING-set subset, so
//! `io.fs` where the oracle proved `io.fs.read` is an `OVER`, not a coarser
//! truth. Every handler below therefore answers upstream's answer EXACTLY where
//! it can — which is everywhere the literal is in the call node, and the call
//! node is all any of them reads — and the caller's fallback for anything this
//! module cannot answer is ∅, never the parent label.
//!
//! `sql_verb` is the one handler with no body here: it serves PLUGIN rows only
//! (`connection.execute` / `exec_query` / `select_all`), `core.yml` has no row
//! for it, and the plugin effect layer is outside ADR-0043 entirely. It answers
//! ∅ — the safe direction — and a test pins that no shipped row names it.

use rigor_parse::ruby_prism::{
    ArgumentsNode, AssocNode, CallNode, InterpolatedStringNode, KeywordHashNode, Node, StringNode,
};

/// Ruby's file modes, minus the encoding suffix (`"r:UTF-8"`) and the `b` / `t`
/// flags (`narrowing.rb:42`). `"r"` is the only pure read; every `+` form reads
/// and writes.
const WRITE_MODES: &[&str] = &["w", "a", "w+", "a+", "r+"];

/// The labels `handler` reads off `node`, or `None` when the handler is one
/// this port does not implement (only `sql_verb`, which no `core.yml` row
/// names). `None` is NOT the parent label — see the module docs.
pub(super) fn apply(handler: &str, node: &CallNode<'_>) -> Option<Vec<String>> {
    Some(match handler {
        "kernel_open" => kernel_open(node),
        "file_open" => mode_labels(node, 1),
        "pathname_open" => mode_labels(node, 0),
        "time_new" => zero_arg(node, "nondet.time"),
        "random_new" => zero_arg(node, "nondet.random"),
        "uri_open" => uri_open(node),
        _ => return None,
    })
}

fn labels(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| (*name).to_string()).collect()
}

/// `Kernel#open` — a path, a `|command` pipe, or (with open-uri loaded) a URI.
/// A literal leading `|` is the pipe form; anything else literal is a path,
/// whose direction the mode literal decides.
fn kernel_open(node: &CallNode<'_>) -> Vec<String> {
    let Some(target) = literal_prefix(positional(node).first()) else {
        return labels(&["io"]);
    };
    if target.starts_with('|') {
        return labels(&["io.process"]);
    }
    // A literal path: the mode literal decides the direction, exactly as
    // `File.open` reads it.
    mode_labels(node, 1)
}

/// `URI.open` / `OpenURI.open_uri` — the scheme literal decides the subsystem. A
/// bare path (no `scheme://`) is open-uri's filesystem fallback; a scheme nobody
/// rowed answers the parent.
fn uri_open(node: &CallNode<'_>) -> Vec<String> {
    let Some(target) = literal_prefix(positional(node).first()) else {
        return labels(&["io"]);
    };
    if target.starts_with("http://") || target.starts_with("https://") {
        labels(&["io.net.http"])
    } else if target.starts_with("file://") {
        labels(&["io.fs.read"])
    } else if target.contains("://") {
        labels(&["io"])
    } else {
        labels(&["io.fs.read"])
    }
}

/// `Time.new` / `Random.new` — with no POSITIONAL arguments it reads the clock
/// (or platform entropy); with any it is constructed from them.
///
/// Keyword arguments do not count: `Time.new(in: "+09:00")` is still now
/// (`narrowing.rb:170`'s `grep_v(KeywordHashNode)`). The probe called counting
/// every argument the UNDER-safe reading for a port riding the LOWERED AST,
/// where the distinction is gone; this collector walks Prism, where a
/// `KeywordHashNode` is a node type, so it answers upstream's rule exactly.
fn zero_arg(node: &CallNode<'_>, label: &str) -> Vec<String> {
    if positional(node).is_empty() { labels(&[label]) } else { Vec::new() }
}

/// The direction a mode argument proves. Three states, and the middle one is
/// the reason this is not a two-way test: **absent** is Ruby's `"r"` default,
/// **present but unreadable** is genuinely unknown and answers the subsystem
/// parent, and **a literal** narrows.
///
/// `index` is 1 for `File.open(path, mode)` and 0 for `Pathname#open(mode)`,
/// where the receiver is the path.
fn mode_labels(node: &CallNode<'_>, index: usize) -> Vec<String> {
    let mode = positional(node)
        .into_iter()
        .nth(index)
        .or_else(|| keyword_argument(node, "mode"));
    let Some(mode) = mode else { return labels(&["io.fs.read"]) };

    let Some(canonical) = string_literal(&mode).and_then(|literal| canonical_mode(&literal)) else {
        return labels(&["io.fs"]);
    };
    if !WRITE_MODES.contains(&canonical.as_str()) {
        return labels(&["io.fs.read"]);
    }
    if canonical.ends_with('+') {
        labels(&["io.fs.read", "io.fs.write"])
    } else {
        labels(&["io.fs.write"])
    }
}

/// Upstream's `/\A[rwa]\+?/` — the leading mode letter and an optional `+`,
/// which is what strips the `b` / `t` flag (`"wb"`) and the `:ENC` suffix
/// (`"r:UTF-8"`). A mode that does not START with `r` / `w` / `a` answers None,
/// i.e. "present but unreadable".
fn canonical_mode(mode: &str) -> Option<String> {
    let mut chars = mode.chars();
    let head = chars.next()?;
    if !matches!(head, 'r' | 'w' | 'a') {
        return None;
    }
    Some(if chars.next() == Some('+') { format!("{head}+") } else { head.to_string() })
}

/// The call's positional arguments — every argument that is not the trailing
/// keyword hash (`narrowing.rb:170`'s `grep_v`).
fn positional<'pr>(node: &CallNode<'pr>) -> Vec<Node<'pr>> {
    let Some(arguments): Option<ArgumentsNode<'pr>> = node.arguments() else {
        return Vec::new();
    };
    arguments
        .arguments()
        .iter()
        .filter(|argument| argument.as_keyword_hash_node().is_none())
        .collect()
}

/// The value of `name:` in the call's keyword hash, or None.
fn keyword_argument<'pr>(node: &CallNode<'pr>, name: &str) -> Option<Node<'pr>> {
    let hash: KeywordHashNode<'pr> = node
        .arguments()?
        .arguments()
        .iter()
        .find_map(|argument| argument.as_keyword_hash_node())?;
    hash.elements().iter().find_map(|element| {
        let assoc: AssocNode<'pr> = element.as_assoc_node()?;
        let key = assoc.key().as_symbol_node()?;
        (key.unescaped() == name.as_bytes()).then(|| assoc.value())
    })
}

fn string_literal(node: &Node<'_>) -> Option<String> {
    let literal: StringNode<'_> = node.as_string_node()?;
    Some(String::from_utf8_lossy(literal.unescaped()).into_owned())
}

/// The literal head of a string argument: the whole thing for a plain literal,
/// and the leading literal run for an interpolated one. `open("|#{cmd}")` and
/// `URI.open("https://#{host}/x")` are the shapes that matter — the part that
/// decides the subsystem is written out even when the rest is computed.
fn literal_prefix(node: Option<&Node<'_>>) -> Option<String> {
    let node = node?;
    if let Some(literal) = node.as_string_node() {
        return Some(String::from_utf8_lossy(literal.unescaped()).into_owned());
    }
    let interpolated: InterpolatedStringNode<'_> = node.as_interpolated_string_node()?;
    let head = interpolated.parts().iter().next()?;
    string_literal(&head)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The labels the handler `handler` reads off the LAST call in `source`.
    fn narrow(handler: &str, source: &str) -> Option<Vec<String>> {
        let result = rigor_parse::parse(source.as_bytes());
        let call = crate::effects::tests_support::first_call(&result.node())
            .expect("the fixture must contain a call");
        apply(handler, &call)
    }

    #[test]
    fn file_open_reads_the_mode_literal() {
        // Every row measured against the pinned oracle in the probe's § 4c.
        assert_eq!(narrow("file_open", "File.open(p)").unwrap(), ["io.fs.read"]);
        assert_eq!(narrow("file_open", r#"File.open(p, "r")"#).unwrap(), ["io.fs.read"]);
        assert_eq!(narrow("file_open", r#"File.open(p, "w")"#).unwrap(), ["io.fs.write"]);
        assert_eq!(narrow("file_open", r#"File.open(p, "a")"#).unwrap(), ["io.fs.write"]);
        assert_eq!(narrow("file_open", r#"File.open(p, "wb")"#).unwrap(), ["io.fs.write"]);
        assert_eq!(narrow("file_open", r#"File.open(p, "r:UTF-8")"#).unwrap(), ["io.fs.read"]);
        assert_eq!(narrow("file_open", r#"File.open(p, mode: "w")"#).unwrap(), ["io.fs.write"]);
        assert_eq!(narrow("file_open", r#"File.open(p, "r+")"#).unwrap(), [
            "io.fs.read",
            "io.fs.write"
        ]);
        assert_eq!(narrow("file_open", r#"File.open(p, "a+")"#).unwrap(), [
            "io.fs.read",
            "io.fs.write"
        ]);
    }

    #[test]
    fn a_mode_the_call_computes_answers_the_subsystem_parent() {
        // The three-state middle: PRESENT but unreadable. `File::RDWR` is an
        // integer flag the scan deliberately does not resolve.
        assert_eq!(narrow("file_open", "File.open(p, mode)").unwrap(), ["io.fs"]);
        assert_eq!(narrow("file_open", "File.open(p, File::RDWR)").unwrap(), ["io.fs"]);
        assert_eq!(narrow("file_open", r#"File.open(p, "x")"#).unwrap(), ["io.fs"]);
    }

    #[test]
    fn pathname_open_reads_the_same_shape_one_argument_left() {
        assert_eq!(narrow("pathname_open", "p.open").unwrap(), ["io.fs.read"]);
        assert_eq!(narrow("pathname_open", r#"p.open("w")"#).unwrap(), ["io.fs.write"]);
        assert_eq!(narrow("pathname_open", "p.open(mode)").unwrap(), ["io.fs"]);
    }

    #[test]
    fn kernel_open_reads_a_leading_pipe_as_a_process() {
        assert_eq!(narrow("kernel_open", r#"open("|ls")"#).unwrap(), ["io.process"]);
        // The leading literal RUN of an interpolated string counts — the pipe
        // is written out even when the command is not.
        assert_eq!(narrow("kernel_open", r##"open("|#{cmd}")"##).unwrap(), ["io.process"]);
        assert_eq!(narrow("kernel_open", r#"open("f.txt")"#).unwrap(), ["io.fs.read"]);
        assert_eq!(narrow("kernel_open", r#"open("f.txt", "w")"#).unwrap(), ["io.fs.write"]);
        // A non-literal target is genuinely unknown: file, pipe or URI.
        assert_eq!(narrow("kernel_open", "open(t)").unwrap(), ["io"]);
        assert_eq!(narrow("kernel_open", r##"open("#{t}")"##).unwrap(), ["io"]);
    }

    #[test]
    fn uri_open_splits_on_the_scheme() {
        assert_eq!(narrow("uri_open", r#"URI.open("https://x/y")"#).unwrap(), ["io.net.http"]);
        assert_eq!(narrow("uri_open", r#"URI.open("http://x/y")"#).unwrap(), ["io.net.http"]);
        assert_eq!(narrow("uri_open", r#"URI.open("file:///tmp/x")"#).unwrap(), ["io.fs.read"]);
        assert_eq!(narrow("uri_open", r#"URI.open("ftp://x/y")"#).unwrap(), ["io"]);
        assert_eq!(narrow("uri_open", r#"URI.open("/tmp/x")"#).unwrap(), ["io.fs.read"]);
        assert_eq!(narrow("uri_open", "URI.open(u)").unwrap(), ["io"]);
    }

    #[test]
    fn time_and_random_read_zero_positional_arguments() {
        assert_eq!(narrow("time_new", "Time.new").unwrap(), ["nondet.time"]);
        assert!(narrow("time_new", "Time.new(2020, 1, 1)").unwrap().is_empty());
        // Keyword arguments are grep_v'd out of the count: still now.
        assert_eq!(narrow("time_new", r#"Time.new(in: "+09:00")"#).unwrap(), ["nondet.time"]);
        assert_eq!(narrow("random_new", "Random.new").unwrap(), ["nondet.random"]);
        assert!(narrow("random_new", "Random.new(42)").unwrap().is_empty());
    }

    #[test]
    fn sql_verb_is_the_one_handler_with_no_body() {
        // Plugin-only, and the plugin effect layer is outside ADR-0043. `None`
        // makes the caller drop the row's labels rather than fall back to the
        // parent `io.db`, which would be an over-claim.
        assert!(narrow("sql_verb", r#"c.execute("SELECT 1")"#).is_none());
        assert!(narrow("no_such_handler", "x.y").is_none());
    }
}
