//! Shared source-outline builder (§12): a plain-data nested symbol tree
//! (classes / modules / methods) reconstructed from the flat lowered AST by
//! BYTE-SPAN CONTAINMENT. Both the LSP `textDocument/documentSymbol` handler and
//! the MCP `outline` tool adapt this into their own wire shapes (0-based UTF-16
//! `DocumentSymbol` ranges vs 1-based line numbers), so the nesting logic lives
//! in exactly one place.

use rigor_parse::{LoweredAst, Node};

/// A source symbol kind (the subset an outline surfaces).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum SymKind {
    Class,
    Module,
    Method,
}

impl SymKind {
    /// Lowercase label for text/JSON consumers (`"class"` / `"module"` / `"method"`).
    pub(crate) fn label(self) -> &'static str {
        match self {
            SymKind::Class => "class",
            SymKind::Module => "module",
            SymKind::Method => "method",
        }
    }
}

/// A nested outline node. Spans are BYTE OFFSETS (half-open `[start, end)`); each
/// consumer converts them to its own position units. `full` is the whole
/// definition span, `sel` the name token (for a method) or the full span.
pub(crate) struct SymNode {
    pub name: String,
    pub kind: SymKind,
    pub full: (usize, usize),
    pub sel: (usize, usize),
    pub children: Vec<SymNode>,
}

/// A pre-tree row harvested from one AST node.
struct Raw {
    name: String,
    kind: SymKind,
    full: (usize, usize),
    sel: (usize, usize),
}

/// Build the nested outline of `ast`: every `ClassDef` / `ModuleDef` / named
/// `Definition` becomes a node, nested by span containment (a method under its
/// class; nested classes nest too). The arena is flat, so nesting is
/// reconstructed structurally — the same approach the toplevel-def / override
/// rules use.
pub(crate) fn build(ast: &LoweredAst) -> Vec<SymNode> {
    let mut raws: Vec<Raw> = Vec::new();
    for (_, node) in ast.iter() {
        match node {
            Node::ClassDef { name, span, .. } if !name.is_empty() => raws.push(Raw {
                name: name.clone(),
                kind: SymKind::Class,
                full: *span,
                sel: *span,
            }),
            Node::ModuleDef { name, span, .. } if !name.is_empty() => raws.push(Raw {
                name: name.clone(),
                kind: SymKind::Module,
                full: *span,
                sel: *span,
            }),
            Node::Definition { name: Some(n), name_span, span, .. } => raws.push(Raw {
                name: n.clone(),
                kind: SymKind::Method,
                full: *span,
                sel: name_span.unwrap_or(*span),
            }),
            _ => {}
        }
    }

    // Sort by start ascending, then by end DESCENDING so a container always
    // precedes the symbols it contains (equal start ⇒ wider first).
    raws.sort_by(|a, b| a.full.0.cmp(&b.full.0).then(b.full.1.cmp(&a.full.1)));

    // Parent index via a containment stack: pop while the top doesn't contain
    // the current span (start is already ≤ by the sort, so test end).
    let mut parent: Vec<Option<usize>> = vec![None; raws.len()];
    let mut stack: Vec<usize> = Vec::new();
    for i in 0..raws.len() {
        while let Some(&top) = stack.last() {
            if raws[top].full.1 >= raws[i].full.1 {
                break;
            }
            stack.pop();
        }
        parent[i] = stack.last().copied();
        stack.push(i);
    }

    let mut children: Vec<Vec<usize>> = vec![Vec::new(); raws.len()];
    let mut roots: Vec<usize> = Vec::new();
    for (i, p) in parent.iter().enumerate() {
        match p {
            Some(p) => children[*p].push(i),
            None => roots.push(i),
        }
    }

    roots.iter().map(|&i| assemble(i, &raws, &children)).collect()
}

/// Materialise the `SymNode` at raw-index `i` and its children.
fn assemble(i: usize, raws: &[Raw], children: &[Vec<usize>]) -> SymNode {
    let r = &raws[i];
    SymNode {
        name: r.name.clone(),
        kind: r.kind,
        full: r.full,
        sel: r.sel,
        children: children[i].iter().map(|&c| assemble(c, raws, children)).collect(),
    }
}
