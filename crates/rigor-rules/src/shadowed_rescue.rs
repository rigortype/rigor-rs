//! `flow.shadowed-rescue-clause` (v0.3.0) — a faithful port of the reference
//! `Rigor::Analysis::CheckRules::ShadowedRescueCollector`.
//!
//! A `rescue` clause of a `begin`/`def` rescue chain is dead when an EARLIER
//! clause of the SAME chain already catches a superclass (or the same class) of
//! every exception class the later clause names. The rule is purely syntactic +
//! class-hierarchy lookups (no Typer), so its false-positive envelope is
//! ancestry-certainty, not flow analysis: it fires ONLY when both clauses'
//! exception references are constants that resolve (against the clause's lexical
//! namespace) to a CLASS with known ancestry — an RBS/registry core class, or a
//! project class whose `class Foo < Bar` superclass chain is discovered. Any
//! unresolved constant, dynamic expression (`rescue klass`), splat
//! (`rescue *ERRORS`), or MODULE makes its clause opaque: an opaque earlier
//! clause contributes no coverage, an opaque later clause never fires.
//!
//! This is a SELF-CONTAINED pass (its own walk over `Node::BeginRescue`
//! clauses), wired into the CLI check path alongside `suppression_marker_diagnostics`
//! so its diagnostics pass through line-suppression / `disable:` filtering. It
//! takes the raw source text because the diagnostic message renders the RAW
//! source slices of the exception expressions and the earlier clauses' 1-based
//! lines (the anchor is the later dead clause's `rescue` keyword).

use rigor_index::CoreIndex;
use rigor_infer::SourceIndex;
use rigor_parse::{LoweredAst, Node, NodeId, RescueClause, Span};

use crate::{catalog, Diagnostic, Severity, FLOW_SHADOWED_RESCUE_CLAUSE};

/// The implicit exception class of a bare `rescue` / `rescue => e` (reference
/// `IMPLICIT_RESCUE_CLASS`).
const IMPLICIT_RESCUE_CLASS: &str = "StandardError";

/// Cycle / depth bound for the project `class Foo < Bar` superclass-chain walk
/// (reference `SUPERCLASS_WALK_LIMIT`).
const SUPERCLASS_WALK_LIMIT: usize = 32;

/// A certified rescue clause: its dead-clause node info plus the CERTIFIED
/// exception class names it rescues. Only produced for a fully certified clause;
/// an opaque clause yields `None` and is skipped from every comparison.
struct ClauseInfo<'a> {
    clause: &'a RescueClause,
    names: Vec<String>,
}

/// Emit `flow.shadowed-rescue-clause` for every provably-shadowed rescue clause.
///
/// Its OWN walk over `Node::BeginRescue` nodes — each begin's clause chain is
/// processed in isolation, so comparisons never cross a nested `begin` (a nested
/// begin lowers to its own `BeginRescue` node with its own `clauses`). `source`
/// is the raw file text (for the RAW exception-expression slices + line numbers);
/// it MUST be the exact bytes the AST was lowered from.
pub fn shadowed_rescue_diagnostics(
    ast: &LoweredAst,
    index: &CoreIndex,
    source_index: &SourceIndex,
    source: &str,
) -> Vec<Diagnostic> {
    let line_starts = line_starts_of(source);
    let severity = catalog(FLOW_SHADOWED_RESCUE_CLAUSE)
        .map(|e| e.default_severity)
        .unwrap_or(Severity::Warning);
    let mut out = Vec::new();

    for (_, node) in ast.iter() {
        let Node::BeginRescue { clauses, span, .. } = node else {
            continue;
        };
        if clauses.len() < 2 {
            continue;
        }
        // The lexical namespace the exception constants resolve against — the
        // enclosing class/module names, outermost first (reference
        // `qualified_prefix`). Reconstructed by span-containment over the arena's
        // ClassDef/ModuleDef nodes (the flat walk carries no nesting stack).
        let prefix = enclosing_prefix(ast, *span);

        // Certify each clause up front (opaque clauses stay `None`).
        let infos: Vec<Option<ClauseInfo>> = clauses
            .iter()
            .map(|clause| clause_info(clause, &prefix, ast, index, source_index, source))
            .collect();

        for (idx, info) in infos.iter().enumerate() {
            if idx == 0 {
                continue;
            }
            let Some(info) = info else { continue };
            if let Some(diag) = check_shadowed(
                info,
                &infos[..idx],
                ast,
                index,
                source_index,
                source,
                &line_starts,
                severity,
            ) {
                out.push(diag);
            }
        }
    }

    out
}

/// Resolve a clause to its certified exception CLASS names, or `None` (opaque).
/// A bare `rescue`/`rescue => e` (no exception designators) names the implicit
/// `StandardError`; any designator that is not a constant, does not resolve, or
/// does not certify as a class makes the whole clause opaque.
fn clause_info<'a>(
    clause: &'a RescueClause,
    prefix: &[String],
    ast: &LoweredAst,
    index: &CoreIndex,
    source_index: &SourceIndex,
    source: &str,
) -> Option<ClauseInfo<'a>> {
    if clause.exceptions.is_empty() {
        return certified_class(IMPLICIT_RESCUE_CLASS, index, source_index).then(|| ClauseInfo {
            clause,
            names: vec![IMPLICIT_RESCUE_CLASS.to_string()],
        });
    }

    let mut names = Vec::with_capacity(clause.exceptions.len());
    for &exc in &clause.exceptions {
        let name = certified_exception_class(exc, prefix, ast, index, source_index, source)?;
        names.push(name);
    }
    Some(ClauseInfo { clause, names })
}

/// The resolved, certified class name for one exception reference, or `None`
/// (opaque). Only constant reads / constant paths participate; the name must
/// resolve lexically and certify as a class with known ancestry.
fn certified_exception_class(
    exc: NodeId,
    prefix: &[String],
    ast: &LoweredAst,
    index: &CoreIndex,
    source_index: &SourceIndex,
    source: &str,
) -> Option<String> {
    let name = resolve_constant(exc, prefix, ast, index, source_index, source)?;
    certified_class(&name, index, source_index).then_some(name)
}

/// Lexical constant resolution: innermost enclosing namespace first, then
/// outward, then top-level — the first candidate any knowledge source (project
/// discovery or RBS/registry) recognises wins. An absolute path (`::Foo`) skips
/// the lexical ladder. `None` for a non-constant designator (splat / local /
/// call → the lowered node is not a `ConstantRead`).
fn resolve_constant(
    exc: NodeId,
    prefix: &[String],
    ast: &LoweredAst,
    index: &CoreIndex,
    source_index: &SourceIndex,
    source: &str,
) -> Option<String> {
    let Node::ConstantRead { name, span } = ast.get(exc) else {
        return None; // splat / local / call / dynamic ⇒ opaque.
    };
    if name.is_empty() {
        return None; // un-namable dynamic constant.
    }

    // An absolute `::Foo` bypasses the lexical ladder. The lowered `name` already
    // has the leading `::` stripped (`constant_path_string`), so detect it from
    // the RAW source slice instead.
    if raw_slice(source, *span).starts_with("::") {
        return known_name(name, index, source_index).then(|| name.clone());
    }

    // Candidates: innermost-qualified first, outward, then the bare name.
    for i in (1..=prefix.len()).rev() {
        let candidate = format!("{}::{}", prefix[..i].join("::"), name);
        if known_name(&candidate, index, source_index) {
            return Some(candidate);
        }
    }
    known_name(name, index, source_index).then(|| name.clone())
}

/// Presence check used to pick the lexical candidate — deliberately broader than
/// certification, so a name that resolves to a project module is FOUND here and
/// then rejected by [`certified_class`] (rather than mis-resolving outward to a
/// same-named core class). Reference `known_name?`.
fn known_name(name: &str, index: &CoreIndex, source_index: &SourceIndex) -> bool {
    source_index.knows_class(name) || index.knows_class(name)
}

/// A name is a certified exception-comparable class when it is either a project
/// class with a discovered `class Foo < Bar` superclass entry, or an RBS/registry
/// class that is not a module. A project-discovered name WITHOUT a superclass
/// entry cannot be told apart from a module (the discovery table records both),
/// so it stays uncertified. Reference `certified_class?`.
fn certified_class(name: &str, index: &CoreIndex, source_index: &SourceIndex) -> bool {
    if source_index.discovered_superclass(name).is_some() {
        return true;
    }
    if source_index.knows_class(name) {
        return false; // a bare `class Foo`/`module Foo` — indistinguishable.
    }
    index.knows_class(name) && !index.is_module(name)
}

/// Fires when EVERY class the later clause names is covered by some earlier
/// certified clause. Reference `check_shadowed`.
#[allow(clippy::too_many_arguments)]
fn check_shadowed(
    info: &ClauseInfo,
    earlier: &[Option<ClauseInfo>],
    ast: &LoweredAst,
    index: &CoreIndex,
    source_index: &SourceIndex,
    source: &str,
    line_starts: &[usize],
    severity: Severity,
) -> Option<Diagnostic> {
    // The certified earlier clauses (opaque ones contribute nothing).
    let comparable: Vec<&ClauseInfo> = earlier.iter().filter_map(|e| e.as_ref()).collect();
    if comparable.is_empty() {
        return None;
    }

    // For each name, the FIRST earlier clause covering it (or bail if uncovered).
    let mut covering: Vec<&ClauseInfo> = Vec::with_capacity(info.names.len());
    for name in &info.names {
        let found = comparable.iter().copied().find(|earlier| {
            earlier
                .names
                .iter()
                .any(|e| covered_by(name, e, index, source_index))
        })?;
        covering.push(found);
    }

    // `covering.uniq` — dedup by clause identity, preserving first-occurrence
    // order (mirrors Ruby `Array#uniq`).
    let mut clauses: Vec<&ClauseInfo> = Vec::new();
    for c in covering {
        let ptr = std::ptr::from_ref(c.clause);
        if !clauses.iter().any(|k| std::ptr::eq(std::ptr::from_ref(k.clause), ptr)) {
            clauses.push(c);
        }
    }

    let clause_src = clause_source(info.clause, ast, source);
    let earlier_rendered: Vec<String> = clauses
        .iter()
        .map(|c| {
            let src = clause_source(c.clause, ast, source);
            let line = line_at(line_starts, c.clause.span.0);
            format!("`{src}' (line {line})")
        })
        .collect();
    let plural = if earlier_rendered.len() > 1 { "s" } else { "" };
    let message = format!(
        "shadowed `{clause_src}': every exception class it names is already caught \
         by the earlier {} clause{plural}, so this clause can never run",
        earlier_rendered.join(" and ")
    );

    let span = info.clause.span;
    Some(Diagnostic {
        rule_id: FLOW_SHADOWED_RESCUE_CLAUSE,
        start_offset: span.0,
        end_offset: span.1,
        message,
        severity,
        source_family: "builtin",
        receiver_type: None,
        method_name: None,
    })
}

/// Is `later` the same class as, or a subclass of, `earlier`? RBS/registry
/// ancestry answers first; `Unknown` (a project-defined class on either side)
/// falls through to the discovered `class Foo < Bar` superclass-chain walk.
/// `Superclass`/`Disjoint` are definitive "no". Reference `covered_by?`.
fn covered_by(later: &str, earlier: &str, index: &CoreIndex, source_index: &SourceIndex) -> bool {
    if later == earlier {
        return true;
    }
    match index.class_ordering(later, earlier) {
        rigor_index::ClassOrdering::Equal | rigor_index::ClassOrdering::Subclass => true,
        rigor_index::ClassOrdering::Unknown => {
            project_chain_covered(later, earlier, index, source_index)
        }
        _ => false,
    }
}

/// Walks `later`'s discovered superclass chain (each parent recorded as written
/// at its `class Foo < Bar` site, resolved against the child's namespace) looking
/// for `earlier` — directly, or through an RBS-known ancestor that orders
/// at-or-below `earlier`. Cycle-guarded and depth-capped; a chain that leaves
/// both knowledge sources stays silent. Reference `project_chain_covered?`.
fn project_chain_covered(
    later: &str,
    earlier: &str,
    index: &CoreIndex,
    source_index: &SourceIndex,
) -> bool {
    let mut current = later.to_string();
    let mut seen: Vec<String> = Vec::new();
    for _ in 0..SUPERCLASS_WALK_LIMIT {
        let Some(parent) = source_index.discovered_superclass(&current) else {
            return false;
        };
        let resolved = resolve_parent(&current, parent, index, source_index);
        if seen.contains(&resolved) {
            return false;
        }
        seen.push(resolved.clone());
        if resolved == earlier {
            return true;
        }
        match index.class_ordering(&resolved, earlier) {
            rigor_index::ClassOrdering::Equal | rigor_index::ClassOrdering::Subclass => return true,
            rigor_index::ClassOrdering::Unknown => current = resolved,
            _ => return false,
        }
    }
    false
}

/// Resolves a superclass name as written (`Error`) against the child's namespace
/// (`Mail::POP3` → `Mail::Error`), longest prefix first; falls back to the
/// as-written name (which RBS may know, e.g. `StandardError`). Reference
/// `resolve_parent`.
fn resolve_parent(
    child: &str,
    parent: &str,
    index: &CoreIndex,
    source_index: &SourceIndex,
) -> String {
    let segments: Vec<&str> = child.split("::").collect();
    // Drop the child's own last component, then try longest namespace prefix.
    for take in (1..segments.len()).rev() {
        let candidate = format!("{}::{}", segments[..take].join("::"), parent);
        if known_name(&candidate, index, source_index) {
            return candidate;
        }
    }
    parent.to_string()
}

/// The rendered `rescue A, B` clause source — `"rescue"` for a bare rescue, else
/// `"rescue "` + the RAW source slices of the exception expressions, comma-joined
/// exactly as written (`exceptions.map(&:slice).join(", ")`). Reference
/// `clause_source`.
fn clause_source(clause: &RescueClause, ast: &LoweredAst, source: &str) -> String {
    if clause.exceptions.is_empty() {
        return "rescue".to_string();
    }
    let parts: Vec<&str> = clause
        .exceptions
        .iter()
        .map(|&id| raw_slice(source, ast.get(id).span()))
        .collect();
    format!("rescue {}", parts.join(", "))
}

/// Byte-slice `[span.0, span.1)` of `source` as a `&str`, empty on a bad range.
fn raw_slice(source: &str, span: Span) -> &str {
    source.get(span.0..span.1).unwrap_or("")
}

/// Precompute the byte offset of every line start (index 0 = line 1).
fn line_starts_of(source: &str) -> Vec<usize> {
    let mut starts = vec![0];
    for (i, b) in source.bytes().enumerate() {
        if b == b'\n' {
            starts.push(i + 1);
        }
    }
    starts
}

/// The 1-based source line of a byte offset (binary search over line starts).
fn line_at(line_starts: &[usize], offset: usize) -> usize {
    line_starts.partition_point(|&ls| ls <= offset)
}

/// The enclosing class/module names for a node at `inner`, outermost first —
/// every `ClassDef`/`ModuleDef` whose span strictly contains `inner`, ordered by
/// containment (widest first). Names are the as-written constant paths, matching
/// the reference `qualified_prefix` (which pushes `qualified_name(constant_path)`
/// per enclosing class/module node).
fn enclosing_prefix(ast: &LoweredAst, inner: Span) -> Vec<String> {
    let mut enclosing: Vec<(Span, &str)> = Vec::new();
    for (_, node) in ast.iter() {
        let (name, span) = match node {
            Node::ClassDef { name, span, .. } | Node::ModuleDef { name, span, .. } => (name, *span),
            _ => continue,
        };
        if name.is_empty() {
            continue;
        }
        // Strictly contains `inner` (and is not `inner` itself).
        if span.0 <= inner.0 && inner.1 <= span.1 && span != inner {
            enclosing.push((span, name.as_str()));
        }
    }
    // Outermost first: smallest start, then largest end.
    enclosing.sort_by(|a, b| a.0 .0.cmp(&b.0 .0).then(b.0 .1.cmp(&a.0 .1)));
    enclosing.into_iter().map(|(_, n)| n.to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rigor_infer::SourceIndex;
    use rigor_parse::{lower, parse};

    /// Run the rule over one source string, returning `(line, column, message)`
    /// per firing (column is 1-based, from the `rescue` keyword offset).
    fn run(src: &str) -> Vec<(usize, usize, String)> {
        let ast = lower(&parse(src.as_bytes()));
        let index = CoreIndex::new();
        let source = SourceIndex::build(&ast, &index);
        let diags = shadowed_rescue_diagnostics(&ast, &index, &source, src);
        // Compute a 1-based column from the byte offset, like the CLI does.
        let starts = line_starts_of(src);
        diags
            .into_iter()
            .map(|d| {
                let line = line_at(&starts, d.start_offset);
                let col = d.start_offset - starts[line - 1] + 1;
                (line, col, d.message)
            })
            .collect()
    }

    fn msgs(src: &str) -> Vec<String> {
        run(src).into_iter().map(|(_, _, m)| m).collect()
    }

    #[test]
    fn standard_error_shadows_argument_error() {
        let out = run("begin\n  x\nrescue StandardError\n  a\nrescue ArgumentError\n  b\nend\n");
        assert_eq!(out.len(), 1);
        let (line, col, msg) = &out[0];
        assert_eq!((*line, *col), (5, 1));
        assert_eq!(
            msg,
            "shadowed `rescue ArgumentError': every exception class it names is already caught \
             by the earlier `rescue StandardError' (line 3) clause, so this clause can never run"
        );
    }

    #[test]
    fn bare_rescue_shadows_as_standard_error() {
        assert_eq!(
            msgs("begin\n  x\nrescue => e\n  a\nrescue ArgumentError\n  b\nend\n"),
            vec![
                "shadowed `rescue ArgumentError': every exception class it names is already caught \
                 by the earlier `rescue' (line 3) clause, so this clause can never run"
                    .to_string()
            ]
        );
    }

    #[test]
    fn exact_dup_fires() {
        assert_eq!(
            msgs("begin\n  x\nrescue ArgumentError\n  a\nrescue ArgumentError\n  b\nend\n").len(),
            1
        );
    }

    #[test]
    fn multi_class_arm_all_covered_by_one_earlier() {
        assert_eq!(
            msgs("begin\n  x\nrescue StandardError\n  a\nrescue ArgumentError, TypeError\n  b\nend\n"),
            vec![
                "shadowed `rescue ArgumentError, TypeError': every exception class it names is \
                 already caught by the earlier `rescue StandardError' (line 3) clause, so this \
                 clause can never run"
                    .to_string()
            ]
        );
    }

    #[test]
    fn multi_earlier_joined_with_and_and_pluralized() {
        let out = msgs(
            "begin\n  x\nrescue ArgumentError\n  a\nrescue TypeError\n  b\nrescue ArgumentError, TypeError\n  c\nend\n",
        );
        assert_eq!(
            out,
            vec![
                "shadowed `rescue ArgumentError, TypeError': every exception class it names is \
                 already caught by the earlier `rescue ArgumentError' (line 3) and `rescue \
                 TypeError' (line 5) clauses, so this clause can never run"
                    .to_string()
            ]
        );
    }

    #[test]
    fn three_clauses_each_shadowed_by_first() {
        let out = run(
            "begin\n  x\nrescue StandardError\n  a\nrescue ArgumentError\n  b\nrescue TypeError\n  c\nend\n",
        );
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].0, 5);
        assert_eq!(out[1].0, 7);
    }

    #[test]
    fn absolute_path_renders_and_resolves() {
        assert_eq!(
            msgs("begin\n  x\nrescue ::StandardError\n  a\nrescue ::ArgumentError\n  b\nend\n"),
            vec![
                "shadowed `rescue ::ArgumentError': every exception class it names is already \
                 caught by the earlier `rescue ::StandardError' (line 3) clause, so this clause \
                 can never run"
                    .to_string()
            ]
        );
    }

    #[test]
    fn def_level_rescue_chain_fires() {
        assert_eq!(
            msgs("def foo\n  x\nrescue StandardError\n  a\nrescue ArgumentError\n  b\nend\n").len(),
            1
        );
    }

    #[test]
    fn exception_then_standard_error_fires() {
        assert_eq!(
            msgs("begin\n  x\nrescue Exception\n  a\nrescue StandardError\n  b\nend\n").len(),
            1
        );
    }

    #[test]
    fn standard_error_then_exception_is_silent() {
        // narrow -> wide (later names a superclass) — the common correct idiom.
        assert!(msgs("begin\n  x\nrescue StandardError\n  a\nrescue Exception\n  b\nend\n").is_empty());
    }

    #[test]
    fn narrow_to_wide_is_silent() {
        assert!(
            msgs("begin\n  x\nrescue ArgumentError\n  a\nrescue StandardError\n  b\nend\n").is_empty()
        );
    }

    #[test]
    fn disjoint_siblings_silent() {
        assert!(msgs("begin\n  x\nrescue ArgumentError\n  a\nrescue TypeError\n  b\nend\n").is_empty());
    }

    #[test]
    fn partial_coverage_silent() {
        // Only ArgumentError is covered; TypeError is not => the arm survives.
        assert!(
            msgs("begin\n  x\nrescue ArgumentError\n  a\nrescue ArgumentError, TypeError\n  b\nend\n")
                .is_empty()
        );
    }

    #[test]
    fn unresolved_constant_is_opaque_silent() {
        assert!(msgs("begin\n  x\nrescue Foo::Bar\n  a\nrescue Foo::Bar\n  b\nend\n").is_empty());
    }

    #[test]
    fn module_designator_never_certifies() {
        assert!(msgs("begin\n  x\nrescue Kernel\n  a\nrescue Kernel\n  b\nend\n").is_empty());
    }

    #[test]
    fn splat_designator_is_opaque() {
        assert!(
            msgs("ERRORS = [ArgumentError]\nbegin\n  x\nrescue StandardError\n  a\nrescue *ERRORS\n  b\nend\n")
                .is_empty()
        );
    }

    #[test]
    fn dynamic_local_designator_is_opaque() {
        assert!(
            msgs("klass = ArgumentError\nbegin\n  x\nrescue StandardError\n  a\nrescue klass\n  b\nend\n")
                .is_empty()
        );
    }

    #[test]
    fn project_class_with_superclass_certifies_and_fires() {
        assert_eq!(
            msgs("class CustomError < StandardError\nend\nbegin\n  x\nrescue StandardError\n  a\nrescue CustomError\n  b\nend\n")
                .len(),
            1
        );
    }

    #[test]
    fn project_class_without_superclass_is_silent() {
        assert!(
            msgs("class CustomError\nend\nbegin\n  x\nrescue StandardError\n  a\nrescue CustomError\n  b\nend\n")
                .is_empty()
        );
    }

    #[test]
    fn project_grandchild_chain_walk_fires() {
        assert_eq!(
            msgs("class Mid < StandardError\nend\nclass Leaf < Mid\nend\nbegin\n  x\nrescue StandardError\n  a\nrescue Leaf\n  b\nend\n")
                .len(),
            1
        );
    }

    #[test]
    fn nested_begin_never_compares_across_chains() {
        assert!(msgs(
            "begin\n  begin\n    x\n  rescue ArgumentError\n    i\n  end\nrescue StandardError\n  o\nend\n"
        )
        .is_empty());
    }

    #[test]
    fn single_clause_never_fires() {
        assert!(msgs("begin\n  x\nrescue StandardError\n  a\nend\n").is_empty());
    }

    #[test]
    fn rescue_inside_block_fires_at_indented_column() {
        let out = run(
            "[1, 2].each do |i|\n  begin\n    x\n  rescue StandardError\n    a\n  rescue ArgumentError\n    b\n  end\nend\n",
        );
        assert_eq!(out.len(), 1);
        assert_eq!((out[0].0, out[0].1), (6, 3));
    }
}
