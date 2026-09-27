//! The `def.*` rules: `def.override-visibility-reduced` (ADR-35) and
//! `def.ivar-write-mismatch`.

use rigor_index::CoreIndex;
use rigor_infer::Typer;
use rigor_parse::{LoweredAst, Node};
use rigor_types::Interner;

use crate::{
    catalog, qualified_class_names, span_within, Diagnostic, Severity, DEF_IVAR_WRITE_MISMATCH,
    DEF_OVERRIDE_VISIBILITY_REDUCED,
};

/// The numeric rank of a visibility under the `public > protected > private`
/// ordering (ADR-35 slice 1). A STRICTLY lower override rank than the parent's
/// is a reduction. Mirrors the reference's `VISIBILITY_RANK`.
fn visibility_rank(v: rigor_parse::Visibility) -> u8 {
    match v {
        rigor_parse::Visibility::Public => 2,
        rigor_parse::Visibility::Protected => 1,
        rigor_parse::Visibility::Private => 0,
    }
}

/// Render a visibility as the reference spells it in the diagnostic message
/// (lowercase, NO colon): `public` / `protected` / `private`.
fn visibility_word(v: rigor_parse::Visibility) -> &'static str {
    match v {
        rigor_parse::Visibility::Public => "public",
        rigor_parse::Visibility::Protected => "protected",
        rigor_parse::Visibility::Private => "private",
    }
}

/// Apply `def.override-visibility-reduced` to one overriding instance method.
///
/// Fires (returns `Some`) iff ALL of these hold — each `None` is a DECLINE (a
/// missed witness, NEVER a false positive):
///
///   1. The override is an instance method present in the visibility table
///      (`override_vis` — singleton defs are excluded upstream by lowering).
///   2. [`SourceIndex::nearest_ancestor_defining`] finds a PROJECT-source
///      ancestor that defines `method` (RBS / third-party ancestors are not
///      walked — slice-1 carve-out; an unresolvable / absent ancestor declines).
///   3. **The parent visibility is KNOWN (`Some`).** We NEVER synthesize `Public`
///      from a missing/absent ancestor visibility entry — this is THE documented
///      false-positive cluster in the reference (Mastodon 160 → 35). Only compare
///      when the nearest defining ancestor genuinely records the method in its
///      visibility table.
///   4. The override's rank is STRICTLY LOWER than the parent's
///      (`rank(override) < rank(parent)`). Same-or-wider (a widening
///      `private→protected`, `protected→public`) declines.
///
/// The diagnostic anchors on the overriding def's name token (`name_span`) and
/// reproduces the reference's byte-exact message:
/// `` visibility of `m' reduced from <parent> to <override> (overrides
/// Parent#m); breaks substitutability ``.
pub(crate) fn check_override_visibility(
    source: &rigor_infer::SourceIndex,
    // The FULLY LEXICALLY-QUALIFIED name of the overriding class (e.g.
    // `Organizations::GroupsController`), so the ancestor walk resolves against
    // the project-wide qualified override index precisely.
    qualified_class: &str,
    method: &str,
    override_vis: rigor_parse::Visibility,
    name_span: (usize, usize),
) -> Option<Diagnostic> {
    // Gate 2: a project ancestor must DEFINE the method.
    let (parent_class, parent_vis) = source.nearest_ancestor_defining(qualified_class, method)?;
    // Gate 3 (the keystone): the parent visibility must be KNOWN — NEVER
    // synthesize Public from a missing entry.
    let parent_vis = parent_vis?;
    // Gate 4: strict reduction only.
    if visibility_rank(override_vis) >= visibility_rank(parent_vis) {
        return None;
    }

    let severity = catalog(DEF_OVERRIDE_VISIBILITY_REDUCED)
        .map(|e| e.default_severity)
        .unwrap_or(Severity::Warning);
    let message = format!(
        "visibility of `{method}' reduced from {} to {} (overrides {parent_class}#{method}); breaks substitutability",
        visibility_word(parent_vis),
        visibility_word(override_vis),
    );
    Some(Diagnostic {
        rule_id: DEF_OVERRIDE_VISIBILITY_REDUCED,
        start_offset: name_span.0,
        end_offset: name_span.1,
        message,
        severity,
        source_family: "builtin",
        receiver_type: None,
        method_name: Some(method.to_string()),
    })
}

// ---------------------------------------------------------------------------
// `def.ivar-write-mismatch` — faithful port of `IvarWriteCollector` +
// `ivar_mismatch_diagnostics_for` + `ivar_class_for`.
// ---------------------------------------------------------------------------

/// One collected `@x = value` write: the rvalue node to type, the `@x` name-token
/// span the diagnostic anchors on, and the write's byte span (for the rescue-scope
/// lookup of increment a).
struct IvarWrite {
    value: rigor_parse::NodeId,
    name_span: rigor_parse::Span,
    span: rigor_parse::Span,
}

/// A resolved rescue binding in effect over a clause body (increment a): within
/// `clause_span`, a read of `bound_name` types to `exception_class`.
struct RescueBinding {
    clause_span: rigor_parse::Span,
    bound_name: String,
    exception_class: String,
}

/// Collect the resolvable rescue bindings: a single-class `rescue C => e` (whose
/// `C` names a core- or project-known class) binds `e` to `C`; a bare `rescue => e`
/// binds `e` to `StandardError`. A multi-class `rescue A, B => e` binds a union
/// the reference cannot name to a single concrete class, so it is NOT recorded
/// (the write stays silent) — probed against the oracle. An unresolvable exception
/// constant is likewise skipped (a coverage gap, FP-safe).
fn collect_rescue_bindings(
    ast: &LoweredAst,
    index: &CoreIndex,
    source: &rigor_infer::SourceIndex,
) -> Vec<RescueBinding> {
    let mut out = Vec::new();
    for (_, n) in ast.iter() {
        let Node::BeginRescue { clauses, .. } = n else {
            continue;
        };
        for clause in clauses {
            let Some(bound_name) = &clause.bound_name else {
                continue;
            };
            let exception_class = match clause.exceptions.as_slice() {
                // Bare `rescue => e` catches `StandardError`.
                [] => {
                    if index.knows_class("StandardError") {
                        Some("StandardError".to_string())
                    } else {
                        None
                    }
                }
                // A single named class — resolvable via core RBS or the project
                // source registry (`rescue MyError => e` where `MyError` is a
                // discovered project class fires too, probed).
                [only] => match ast.get(*only) {
                    Node::ConstantRead { name, .. }
                        if !name.is_empty()
                            && (index.knows_class(name) || source.knows_class(name)) =>
                    {
                        Some(name.clone())
                    }
                    _ => None,
                },
                // Multi-class arm ⇒ a union with no single concrete class ⇒ silent.
                _ => None,
            };
            if let Some(exception_class) = exception_class {
                out.push(RescueBinding {
                    clause_span: clause.span,
                    bound_name: bound_name.clone(),
                    exception_class,
                });
            }
        }
    }
    out
}

/// The concrete class NAME of one ivar write's rvalue — the `ivar_class_for`
/// analog. Increment (a): a read of a rescue-bound variable inside the clause body
/// resolves to the exception class directly (bypassing the `TypeId` layer, since
/// exception classes are outside the 9-class `Nominal` id space). Otherwise the
/// rvalue is typed through the shared typer against an EMPTY local env (literals
/// and the `Integer()`/`Float()`/`String()` folds are env-independent; any other
/// local read declines to `Dynamic` ⇒ `None`, a coverage gap that can only silence
/// the rule, never mis-fire it) and mapped via `class_name_of`, with
/// `TrueClass`/`FalseClass` folded to `"bool"`.
fn ivar_write_class(
    ast: &LoweredAst,
    write: &IvarWrite,
    typer: &Typer,
    index: &CoreIndex,
    interner: &mut Interner,
    rescue_bindings: &[RescueBinding],
) -> Option<String> {
    if let Node::LocalVariableRead { name, .. } = ast.get(write.value) {
        for binding in rescue_bindings {
            if binding.bound_name == *name && span_within(write.span, binding.clause_span) {
                return Some(binding.exception_class.clone());
            }
        }
    }
    let env = rigor_infer::TypeEnv::new();
    let ty = typer.type_of(ast, write.value, &env, interner);
    match index.class_name_of(interner, ty) {
        Some("TrueClass") | Some("FalseClass") => Some("bool".to_string()),
        Some(name) => Some(name.to_string()),
        None => None,
    }
}

/// Emit every `def.ivar-write-mismatch` diagnostic for one file. Walks each
/// ClassDef/ModuleDef reachable through class/module bodies (a class nested in a
/// `def` is absent from `qualified_class_names`, matching the reference walk that
/// returns at the first `def`), collects its DIRECT instance-`def` bodies' plain
/// `@x = value` writes (barriers at nested def/class/module; singleton `def self.x`
/// and non-instance defs skipped), groups by (qualified class, ivar) in source
/// order, then applies the reference firing logic.
pub(crate) fn ivar_write_mismatch_diagnostics(
    ast: &LoweredAst,
    interner: &mut Interner,
    index: &CoreIndex,
    source: &rigor_infer::SourceIndex,
    typer: &Typer,
    out: &mut Vec<Diagnostic>,
) {
    let qualified = qualified_class_names(ast);

    // Every def/class/module span — the write barriers.
    let barrier_spans: Vec<rigor_parse::Span> = ast
        .iter()
        .filter_map(|(_, n)| match n {
            Node::Definition { span, .. }
            | Node::ClassDef { span, .. }
            | Node::ModuleDef { span, .. } => Some(*span),
            _ => None,
        })
        .collect();

    let rescue_bindings = collect_rescue_bindings(ast, index, source);

    // Gather writes grouped by (qualified class, ivar), preserving first-seen
    // (source) order.
    let mut order: Vec<(String, String)> = Vec::new();
    let mut groups: std::collections::HashMap<(String, String), Vec<IvarWrite>> =
        std::collections::HashMap::new();

    for (class_id, node) in ast.iter() {
        let body = match node {
            Node::ClassDef { body, .. } | Node::ModuleDef { body, .. } => body,
            _ => continue,
        };
        let Some(class_name) = qualified.get(&class_id) else {
            continue; // un-namable / nested-in-def ⇒ never collected.
        };
        for &child_id in body {
            let Node::Definition {
                name: Some(_),
                span: def_span,
                ..
            } = ast.get(child_id)
            else {
                continue; // singleton / non-instance def ⇒ barrier, skip.
            };
            let def_span = *def_span;
            for (_, wn) in ast.iter() {
                let Node::InstanceVariableWrite {
                    name,
                    value,
                    name_span,
                    span,
                } = wn
                else {
                    continue;
                };
                if !span_within(*span, def_span) {
                    continue;
                }
                // Exclude a write inside a nested def/class/module within this def.
                let barriered = barrier_spans.iter().any(|b| {
                    *b != def_span && span_within(*b, def_span) && span_within(*span, *b)
                });
                if barriered {
                    continue;
                }
                let key = (class_name.clone(), name.clone());
                if !groups.contains_key(&key) {
                    order.push(key.clone());
                }
                groups.entry(key).or_default().push(IvarWrite {
                    value: *value,
                    name_span: *name_span,
                    span: *span,
                });
            }
        }
    }

    let severity = catalog(DEF_IVAR_WRITE_MISMATCH)
        .map(|e| e.default_severity)
        .unwrap_or(Severity::Warning);

    for (class_name, ivar_name) in &order {
        let writes = &groups[&(class_name.clone(), ivar_name.clone())];
        if writes.len() < 2 {
            continue;
        }
        // The class string of every write (mapped `ivar_class_for`).
        let mut classes: Vec<Option<String>> = Vec::with_capacity(writes.len());
        for w in writes {
            classes.push(ivar_write_class(ast, w, typer, index, interner, &rescue_bindings));
        }

        // Canonical = first write whose class is not "NilClass" (leading `@x = nil`
        // placeholders skipped). If that write's class is unresolvable (`None`),
        // the WHOLE group is silent.
        let Some(canonical) = classes
            .iter()
            .position(|c| c.as_deref() != Some("NilClass"))
        else {
            continue;
        };
        let Some(first_class) = classes[canonical].clone() else {
            continue;
        };

        for i in (canonical + 1)..writes.len() {
            let Some(other_class) = &classes[i] else {
                continue;
            };
            if other_class == "NilClass" || *other_class == first_class {
                continue; // clear-to-nil idiom / same class ⇒ silent.
            }
            let w = &writes[i];
            out.push(Diagnostic {
                rule_id: DEF_IVAR_WRITE_MISMATCH,
                start_offset: w.name_span.0,
                end_offset: w.name_span.1,
                message: format!(
                    "instance variable `{ivar_name}' on {class_name} was previously \
                     assigned {first_class}; this write assigns {other_class}"
                ),
                severity,
                source_family: "builtin",
                receiver_type: None,
                method_name: None,
            });
        }
    }
}
