//! Diagnostic rules + the structured `Diagnostic` type (ADR-0014: rule id,
//! severity, primary/secondary annotations, subdiagnostics). All rules run in a
//! single converged AST walk (ADR-0005), not one pass per rule. The tracer
//! bullet's first rule is `call.undefined-method`.
#![allow(dead_code)]

use rigor_index::CoreIndex;
use rigor_infer::Typer;
use rigor_parse::{LoweredAst, Node};
use rigor_types::{Interner, Scalar, Type};

// ---------------------------------------------------------------------------
// Severity enum
// ---------------------------------------------------------------------------

/// The three severity levels (ADR-0030). Matches the reference's
/// `:error` / `:warning` / `:info` atoms.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
    Info,
}

impl Severity {
    /// Render as the reference spells it in JSON/text output.
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Info => "info",
        }
    }
}

// ---------------------------------------------------------------------------
// Diagnostic struct
// ---------------------------------------------------------------------------

/// A diagnostic finding, identified by `rule_id` + location (ADR-0002 parity
/// is defined over this pair).
///
/// `receiver_type` and `method_name` are omitted from the struct (None) for
/// rules that don't operate on a call dispatch subject.
///
/// # TODO(spec)
/// - `project_definition_site: Option<String>` — `"path:line"` for
///   `call.undefined-method` when the project defines the called method via a
///   monkey-patch or `pre_eval:`. Set by `call.undefined-method` once the
///   project-index layer is implemented (ADR-0017).
#[derive(Clone, Debug)]
pub struct Diagnostic {
    pub rule_id: &'static str,
    pub start_offset: usize,
    pub end_offset: usize,
    pub message: String,
    /// Authored severity before any profile re-stamping.
    pub severity: Severity,
    /// Identifies the rule source: `"builtin"` for all rules shipped with
    /// rigor-rs. Future values: `"plugin.<id>"`, `"rbs_extended"`,
    /// `"generated.<provider>"` (ADR-0030).
    ///
    /// # TODO(spec)
    /// Implement the full source_family set once plugins / RBS extensions land.
    pub source_family: &'static str,
    /// Rendered receiver class/type for call/def rules; `None` for other rules.
    pub receiver_type: Option<String>,
    /// Called / defined method name for call/def rules; `None` otherwise.
    pub method_name: Option<String>,
}

// ---------------------------------------------------------------------------
// Rule catalogue
// ---------------------------------------------------------------------------

/// Per-rule static properties that enrich the JSON output stream but are NOT
/// carried on the `Diagnostic` object itself (ADR-0030 / reference ADR-65).
pub struct RuleEntry {
    pub default_severity: Severity,
    /// Confidence tier for consumers routing attention: `"high"` | `"medium"` |
    /// `"low"`. Omitted (None) for informational / plugin rules.
    pub evidence_tier: &'static str,
    /// Stable per-rule documentation URL.
    pub documentation_url: &'static str,
}

/// Static catalogue of the three rules implemented in this slice.
///
/// `catalog(rule_id)` returns the entry for a known rule, `None` for unknown.
pub fn catalog(rule_id: &str) -> Option<&'static RuleEntry> {
    match rule_id {
        CALL_UNDEFINED_METHOD => Some(&RuleEntry {
            default_severity: Severity::Error,
            evidence_tier: "high",
            documentation_url: "https://github.com/rigortype/rigor/blob/main/docs/manual/04-diagnostics.md#rule-call-undefined-method",
        }),
        CALL_WRONG_ARITY => Some(&RuleEntry {
            default_severity: Severity::Error,
            evidence_tier: "high",
            documentation_url: "https://github.com/rigortype/rigor/blob/main/docs/manual/04-diagnostics.md#rule-call-wrong-arity",
        }),
        CALL_POSSIBLE_NIL_RECEIVER => Some(&RuleEntry {
            default_severity: Severity::Warning,
            evidence_tier: "medium",
            documentation_url: "https://github.com/rigortype/rigor/blob/main/docs/manual/04-diagnostics.md#rule-call-possible-nil-receiver",
        }),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Rule IDs
// ---------------------------------------------------------------------------

/// The stable id of the headline tracer-bullet rule (ADR-0030 taxonomy).
pub const CALL_UNDEFINED_METHOD: &str = "call.undefined-method";

/// `call.wrong-arity`: a call passes a positional-argument count outside the
/// method's known arity envelope (ADR-0030 taxonomy).
pub const CALL_WRONG_ARITY: &str = "call.wrong-arity";

/// `call.possible-nil-receiver`: a call whose receiver may be nil on some path
/// (ADR-0030 taxonomy). In this slice only the union case is in scope; the
/// literal-`nil` case is owned by `call.undefined-method` (matching the
/// reference, which routes `nil.foo` to undefined-method).
pub const CALL_POSSIBLE_NIL_RECEIVER: &str = "call.possible-nil-receiver";

// ---------------------------------------------------------------------------
// analyze()
// ---------------------------------------------------------------------------

/// Analyze a lowered AST and return all diagnostics, in source order.
///
/// This is the single converged walk (ADR-0005): it builds the top-level type
/// environment once, then visits every node, applying every call rule
/// (`call.undefined-method`, `call.wrong-arity`, `call.possible-nil-receiver`)
/// in the SAME pass. At most one diagnostic is emitted per call site, matching
/// the reference's one-diagnostic-per-offending-call discipline.
pub fn analyze(ast: &LoweredAst, interner: &mut Interner, index: &CoreIndex) -> Vec<Diagnostic> {
    // Single-file API: build a per-file source index then delegate. Preserves the
    // existing signature + tests. The project pass (the CLI) builds ONE
    // project-wide source over all files and calls `analyze_with_source` directly.
    let source = rigor_infer::SourceIndex::build(ast, index);
    analyze_with_source(ast, interner, index, &source)
}

/// Analyze a lowered AST against an EXTERNALLY-built [`SourceIndex`] — the
/// project-wide variant the CLI builds once over every file. Splitting this out
/// lets the bare-constant singleton gate (`!source.knows_class(name)`) see class
/// names defined in OTHER files, so a project model referenced in a file that
/// does not define it (`Group.where(...)`) is never singleton-typed and stays
/// silent (the cross-file zero-FP keystone).
pub fn analyze_with_source(
    ast: &LoweredAst,
    interner: &mut Interner,
    index: &CoreIndex,
    source: &rigor_infer::SourceIndex,
) -> Vec<Diagnostic> {
    // A typer over the real RBS index AND the (project-wide) source index, so
    // `X.new` types to an instance and a bare constant `X` types to its class
    // object (`Singleton(X)`) for class-method witnessing. The source index also
    // drives RETURN-TYPE inference for chaining.
    let typer = Typer::with_source(index, source);
    let env = typer.build_toplevel_env(ast, interner);
    let mut out = Vec::new();

    // Visit nodes in id order, which is source-discovery order, so diagnostics
    // come out deterministically (ADR-0020 determinism).
    let calls: Vec<_> = ast
        .iter()
        .filter_map(|(id, node)| match node {
            Node::Call {
                receiver: Some(recv),
                method,
                args,
                block_body,
                message_span,
                ..
            } => Some((
                id,
                *recv,
                method.clone(),
                args.clone(),
                !block_body.is_empty(),
                *message_span,
            )),
            _ => None,
        })
        .collect();

    for (_id, recv, method, args, has_block, message_span) in calls {
        // Rule precedence at one call site (avoid double-emit):
        //   1. undefined-method  (method absent on the receiver class, incl. nil)
        //   2. wrong-arity       (method present but arg count out of envelope)
        //   3. possible-nil-receiver (union receiver with a nil arm)
        // The reference emits exactly one of these per call; we mirror that by
        // returning the first that fires.
        let diag = check_call(ast, recv, &method, message_span, &env, &typer, interner, index)
            .or_else(|| {
                check_wrong_arity(ast, recv, &method, &args, has_block, message_span, &env, &typer, interner, index)
            })
            .or_else(|| {
                check_nil_receiver(ast, recv, &method, message_span, &env, &typer, interner, index)
            });
        if let Some(diag) = diag {
            out.push(diag);
        }
    }

    out
}

// ---------------------------------------------------------------------------
// Rule implementations
// ---------------------------------------------------------------------------

/// Apply `call.undefined-method` to a single call with a receiver.
///
/// Zero-false-positive gate (ADR-0023): emit *only* when the receiver's concrete
/// class is **RBS-known in the core surface** AND that class is known to lack the
/// method. If the receiver is `Dynamic`/unknown, or its class is a project-defined
/// (in-source) or non-core `.new` instance, emit nothing — never guess.
///
/// ## Why in-source / non-core `.new` instances are NOT witnessed
///
/// The reference gates this rule on `rbs_class_known?(class_name)`
/// (`check_rules.rb:556`): a project-defined class — or a non-core class reached
/// only through `X.new` — is treated **leniently**. A method MISS on such a
/// receiver stays `Dynamic[top]` and silent, because Ruby routinely defines
/// methods dynamically (ADR-0023 tier-4: "on a miss, the call stays Dynamic").
/// Empirically the reference is silent on `Point.new.typo`, `MyError.new.typo`,
/// `Pathname.new.typo`, `Set.new.typo`, and `Struct.new(...).new`, while it DOES
/// witness on literals, RBS-method returns, and core `X.new` (`Array.new.typo`).
///
/// The in-source/registry surface ([`rigor_infer::SourceIndex`]) still types such
/// instances — for chained RETURN inference and `X.new` identity — but it is
/// never a *witnessing* surface for this rule. Honouring that boundary is the
/// keystone that keeps real project code (incl. Rails models) false-positive-free.
fn check_call(
    ast: &LoweredAst,
    receiver: rigor_parse::NodeId,
    method: &str,
    message_span: (usize, usize),
    env: &rigor_infer::TypeEnv,
    typer: &Typer,
    interner: &mut Interner,
    index: &CoreIndex,
) -> Option<Diagnostic> {
    let recv_ty = typer.type_of(ast, receiver, env, interner);

    // Singleton (class-object) receiver: a bare constant `C` typed to
    // `Type::Singleton(class)` (see the typer's `ConstantRead` arm + its zero-FP
    // gate). Witness a CLASS-method typo (`Time.current`) against the RBS
    // class-method surface. This branch MUST come first: `class_name_of` returns
    // `None` for a Singleton carrier, so the instance path below would skip it.
    if let Type::Singleton(class) = interner.get(recv_ty) {
        let class = *class;
        let Some(name) = typer.source().class_name_for_id(class) else {
            return None; // not round-trippable ⇒ silent (never guess).
        };
        // `class_has_singleton_method` is conservative: `false` only when the
        // class-method surface is fully known and lacks the method (handles
        // `extend`ed modules; incomplete/unknown ⇒ `true` ⇒ silent).
        if index.class_has_singleton_method(name, method) {
            return None;
        }
        let receiver_render = format!("singleton({name})");
        let message = format!("undefined method `{method}' for {receiver_render}");
        let severity = catalog(CALL_UNDEFINED_METHOD)
            .map(|e| e.default_severity)
            .unwrap_or(Severity::Error);
        return Some(Diagnostic {
            rule_id: CALL_UNDEFINED_METHOD,
            start_offset: message_span.0,
            end_offset: message_span.1,
            message,
            severity,
            source_family: "builtin",
            receiver_type: Some(receiver_render),
            method_name: Some(method.to_string()),
        });
    }

    // Witness ONLY over a class the core (RBS/CORE_CLASSES) surface models and
    // round-trips by id. A receiver that resolves only through the in-source /
    // registry surface (a project class, or a non-core `X.new` like Pathname)
    // returns `None` here ⇒ silent (reference leniency, see the rustdoc above).
    let class_name = index.class_name_of(interner, recv_ty)?;
    if !index.knows_class(class_name) {
        return None;
    }
    if index.class_has_method(class_name, method) {
        return None;
    }

    // We have witnessed absence over a core/RBS class.
    let class_name = class_name.to_string();

    // Render the receiver in the reference's value-in-message style: the bare
    // value for a `Constant` (`"Hello"`, `3`), else the class name. The
    // `message` field is presentation, not contract (ADR-0030).
    let receiver_render = render_receiver(interner, recv_ty, &class_name);
    let message = format!("undefined method `{method}' for {receiver_render}");

    let severity = catalog(CALL_UNDEFINED_METHOD)
        .map(|e| e.default_severity)
        .unwrap_or(Severity::Error);

    // `receiver_type` in the structured field matches the reference's rendering:
    // for a Constant receiver it is the rendered value (e.g. `"\"Hello\""` for
    // a String literal, `"nil"` for nil), not the bare class name. This matches
    // the reference's JSON output which sets `receiver_type` to `"\"Hello\""`.
    Some(Diagnostic {
        rule_id: CALL_UNDEFINED_METHOD,
        start_offset: message_span.0,
        end_offset: message_span.1,
        message,
        severity,
        source_family: "builtin",
        receiver_type: Some(receiver_render),
        method_name: Some(method.to_string()),
    })
}

/// Apply `call.wrong-arity` to a single call with a receiver.
///
/// Zero-false-positive gate (ADR-0023), mirroring the reference's conservative
/// envelope: emit *only* when
///   - the receiver types to a concrete class the [`CoreIndex`] models,
///   - that class is known to DEFINE the method (so this is genuinely an arity
///     violation, not an undefined method — that's the other rule's job),
///   - [`rigor_index::method_arity`] returns a known `(min, max)` envelope, AND
///   - the positional-argument count is definitely outside `[min, max]`.
///
/// A variadic method (`max == None`) only triggers on `args < min`. Any
/// Dynamic / unknown receiver, unmodeled method, or unmodeled arity => silent.
fn check_wrong_arity(
    ast: &LoweredAst,
    receiver: rigor_parse::NodeId,
    method: &str,
    args: &[rigor_parse::NodeId],
    has_block: bool,
    message_span: (usize, usize),
    env: &rigor_infer::TypeEnv,
    typer: &Typer,
    interner: &mut Interner,
    index: &CoreIndex,
) -> Option<Diagnostic> {
    // A block selects a DIFFERENT RBS overload, which usually has a different
    // positional arity (`arr.select { } / arr.map { }` take 0 positional args,
    // but the no-block envelope spans the Enumerator overloads). The reference
    // DOES witness block-form arity by reading the block overload's own arity;
    // we only store a single arity envelope collapsed over ALL overloads, so we
    // cannot isolate the block overload's positional count here. Rather than
    // witness against the wrong (collapsed) envelope — which would risk a false
    // positive — we stay silent on arity for any block-bearing call. This is the
    // zero-FP-safe conservative choice (a missed witness, never an extra one);
    // block-form RETURN typing IS modeled (see `Typer::type_block_call`), so
    // chained undefined-method on a block result is still witnessed — only the
    // block-call's own arity is deferred until per-overload arity is stored.
    if has_block {
        return None;
    }

    let recv_ty = typer.type_of(ast, receiver, env, interner);

    // Resolve the receiver's class; `None` => Dynamic/unknown => silent.
    let class_name = index.class_name_of(interner, recv_ty)?;
    if !index.knows_class(class_name) {
        return None;
    }
    // Only check arity for a method the class actually defines — otherwise the
    // undefined-method rule owns this call site (no double-emit).
    if !index.class_has_method(class_name, method) {
        return None;
    }

    // A known arity envelope is required — never guess on an unmodeled method.
    let (min, max) = index.method_arity(class_name, method)?;

    let given = args.len();
    let too_few = given < min;
    let too_many = max.is_some_and(|m| given > m);
    if !(too_few || too_many) {
        return None;
    }

    // Render the expected envelope the reference's way: a bare count when the
    // arity is fixed (`min == max`), else `min..max`. A variadic upper bound is
    // not reachable here (too_few only), but render it defensively as `min..`.
    let expected = match max {
        Some(m) if m == min => min.to_string(),
        Some(m) => format!("{min}..{m}"),
        None => format!("{min}.."),
    };
    let message = format!(
        "wrong number of arguments to `{method}' on {class_name} (given {given}, expected {expected})"
    );

    let severity = catalog(CALL_WRONG_ARITY)
        .map(|e| e.default_severity)
        .unwrap_or(Severity::Error);

    Some(Diagnostic {
        rule_id: CALL_WRONG_ARITY,
        start_offset: message_span.0,
        end_offset: message_span.1,
        message,
        severity,
        source_family: "builtin",
        receiver_type: Some(class_name.to_string()),
        method_name: Some(method.to_string()),
    })
}

/// Apply `call.possible-nil-receiver` to a single call with a receiver.
///
/// In this slice the rule is intentionally narrow: it fires only when the
/// receiver type proves nil-on-some-path via a UNION carrier containing a nil
/// member, while a non-nil arm still defines the method. The literal-`nil`
/// receiver (`x = nil; x.upcase`) is deliberately NOT handled here — that types
/// to exactly `Constant[Nil]` and is owned by `call.undefined-method` (matching
/// the reference, which routes a definitely-nil receiver to undefined-method).
///
/// The tracer-bullet type lattice does not yet produce union receivers through
/// flow narrowing, so this rule is currently inert on the corpus; it exists to
/// hold the rule id + gate shape without ever introducing a false positive.
//
// TODO(spec): nil-on-a-live-path via union/flow narrowing — once a guard like
// `return if x.nil?` or a `T | nil` parameter produces a `Type::Union` with a
// nil arm, fire here when the called method exists on the non-nil arm(s) but
// not on NilClass (ADR-0022 flow narrowing).
fn check_nil_receiver(
    _ast: &LoweredAst,
    _receiver: rigor_parse::NodeId,
    _method: &str,
    _message_span: (usize, usize),
    _env: &rigor_infer::TypeEnv,
    _typer: &Typer,
    _interner: &mut Interner,
    _index: &CoreIndex,
) -> Option<Diagnostic> {
    // No union/flow narrowing in this slice => never fires. Returning `None`
    // keeps the zero-false-positive contract until flow narrowing lands.
    None
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Render the receiver for the diagnostic message: the bare literal value for a
/// value-pinned `Constant`, else the resolved class name.
fn render_receiver(interner: &Interner, ty: rigor_types::TypeId, class_name: &str) -> String {
    match interner.get(ty) {
        Type::Constant(scalar) => render_scalar(scalar),
        _ => class_name.to_string(),
    }
}

/// Render a scalar literal as it appears in the reference's message: strings
/// quoted (`"Hello"`), symbols colon-prefixed (`:foo`), everything else by its
/// natural literal spelling.
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

// ---------------------------------------------------------------------------
// In-source diagnostic suppression (reference `filter_suppressed`)
// ---------------------------------------------------------------------------

use std::collections::{HashMap, HashSet};

/// The sentinel rule id of the synthetic internal-error diagnostic emitted on a
/// per-file panic (ADR-0016). Such diagnostics carry no real rule and MUST NEVER
/// be suppressed — they represent failures the user cannot silence away (matches
/// the reference's `rule == nil` guard in `filter_suppressed`).
const INTERNAL_ERROR_RULE: &str = "internal-error";

/// Family-wildcard tokens (`call`, `flow`, …). A token in this set expands to
/// every canonical rule whose id starts with `<token>.` (reference
/// `RULE_FAMILIES`). Only `call` can match an implemented rule today; the rest
/// are carried for forward-compat with the reference's catalogue.
const RULE_FAMILIES: &[&str] = &["call", "flow", "assert", "dump", "def"];

/// The canonical rule ids rigor-rs can actually emit. Family expansion and the
/// `disable all` wildcard are checked against this set, so a `call` family token
/// only ever expands to these three (the reference expands against its full
/// `ALL_RULES`, but the extra ids it would add match no rigor-rs diagnostic).
const IMPLEMENTED_RULES: &[&str] =
    &[CALL_UNDEFINED_METHOD, CALL_WRONG_ARITY, CALL_POSSIBLE_NIL_RECEIVER];

/// Maps a legacy short alias to its canonical id (reference `LEGACY_RULE_ALIASES`).
/// Only the three implemented ids can ever match a real diagnostic; the remaining
/// aliases are included for forward-compat (they expand to ids no rigor-rs
/// diagnostic carries, so they are inert).
fn legacy_alias(token: &str) -> Option<&'static str> {
    match token {
        "undefined-method" => Some(CALL_UNDEFINED_METHOD),
        "self-undefined-method" => Some("call.self-undefined-method"),
        "wrong-arity" => Some(CALL_WRONG_ARITY),
        "argument-type-mismatch" => Some("call.argument-type-mismatch"),
        "possible-nil-receiver" => Some(CALL_POSSIBLE_NIL_RECEIVER),
        "dump-type" => Some("dump.type"),
        "assert-type" => Some("assert.type-mismatch"),
        "always-raises" => Some("flow.always-raises"),
        "unreachable-branch" => Some("flow.unreachable-branch"),
        "method-visibility-mismatch" => Some("def.method-visibility-mismatch"),
        "ivar-write-mismatch" => Some("def.ivar-write-mismatch"),
        "dead-assignment" => Some("flow.dead-assignment"),
        "always-truthy-condition" => Some("flow.always-truthy-condition"),
        "unreachable-clause" => Some("flow.unreachable-clause"),
        _ => None,
    }
}

/// A parsed suppression set: a flag for the `all` wildcard plus the explicit
/// canonical rule ids. Mirrors the reference's `Set` that may contain the
/// `"all"` sentinel alongside real ids.
///
/// This is the single source of truth for rule-token expansion (legacy aliases,
/// the `call`/`flow`/… family wildcards, canonical ids, and the `all` wildcard).
/// It backs BOTH in-source `# rigor:disable` suppression and the `.rigor.yml`
/// `disable:` config key, so the two stay in lockstep.
#[derive(Default, Clone)]
pub struct SuppressSet {
    all: bool,
    rules: HashSet<String>,
}

impl SuppressSet {
    /// Build a set from a list of user-supplied rule tokens (e.g. a config
    /// `disable:` list), expanding each through the same logic as inline
    /// `# rigor:disable` directives. The internal-error sentinel can never be
    /// matched here — even an explicit `internal-error`/`all` token leaves it
    /// reportable (enforced by [`SuppressSet::suppresses`]).
    #[must_use]
    pub fn from_tokens<S: AsRef<str>>(tokens: &[S]) -> Self {
        let mut set = Self::default();
        for token in tokens {
            set.absorb_token(token.as_ref());
        }
        set
    }

    /// Whether this set matches `rule` (so the diagnostic should be dropped). The
    /// `internal-error` sentinel is NEVER matched, regardless of `all` or an
    /// explicit token — it represents a failure the user cannot silence (reference
    /// `rule == nil` guard).
    #[must_use]
    pub fn suppresses(&self, rule: &str) -> bool {
        if rule == INTERNAL_ERROR_RULE {
            return false;
        }
        self.all || self.rules.contains(rule)
    }

    fn is_empty(&self) -> bool {
        !self.all && self.rules.is_empty()
    }

    /// Expand one user token into this set (reference `expand_token` +
    /// `absorb_suppression_tokens`).
    fn absorb_token(&mut self, token: &str) {
        if token == "all" {
            self.all = true;
        } else if let Some(canonical) = legacy_alias(token) {
            self.rules.insert(canonical.to_string());
        } else if RULE_FAMILIES.contains(&token) {
            let prefix = format!("{token}.");
            for rule in IMPLEMENTED_RULES {
                if rule.starts_with(&prefix) {
                    self.rules.insert((*rule).to_string());
                }
            }
        } else {
            // Canonical id → itself; unknown token → passes through verbatim
            // (matches no real diagnostic ⇒ a no-op). Both paths just insert
            // the token, matching the reference's `expand_token` fallthrough.
            self.rules.insert(token.to_string());
        }
    }
}

/// Drop the diagnostics suppressed by the file's inline `# rigor:disable` /
/// `# rigor:disable-file` comments, mirroring the reference's `filter_suppressed`
/// (honored regardless of any config file). Each input is `(line, diagnostic)`
/// where `line` is the diagnostic's 1-based source line; `comments` is the
/// `(line, text)` list from [`rigor_parse::comment_lines`].
///
/// A diagnostic is dropped iff its `rule_id` is in the file-suppression set (or
/// that set contains `all`), OR its `rule_id` is in its line's suppression set
/// (or that line's set contains `all`). The internal-error sentinel is never
/// dropped.
#[must_use]
pub fn filter_suppressed(
    diagnostics: Vec<(usize, Diagnostic)>,
    comments: &[(usize, String)],
) -> Vec<(usize, Diagnostic)> {
    let (line_suppressions, file_suppressions) = parse_suppression_comments(comments);

    diagnostics
        .into_iter()
        .filter(|(line, diag)| {
            // Never suppress the internal-error sentinel (reference: `rule.nil?`).
            if diag.rule_id == INTERNAL_ERROR_RULE {
                return true;
            }
            if file_suppressions.suppresses(diag.rule_id) {
                return false;
            }
            if let Some(set) = line_suppressions.get(line) {
                if set.suppresses(diag.rule_id) {
                    return false;
                }
            }
            true
        })
        .collect()
}

/// Parse the comment list into `(line_suppressions, file_suppressions)`.
/// File-level directives (`# rigor:disable-file ...`) apply to every line; the
/// `-file` form is checked FIRST so a `disable-file` comment is not also read as
/// a line-level `disable` (the reference's `(?!-file)` negative lookahead).
fn parse_suppression_comments(
    comments: &[(usize, String)],
) -> (HashMap<usize, SuppressSet>, SuppressSet) {
    let mut line_suppressions: HashMap<usize, SuppressSet> = HashMap::new();
    let mut file_suppressions = SuppressSet::default();

    for (line, text) in comments {
        if let Some(rules) = match_directive(text, "rigor:disable-file") {
            absorb_tokens(rules, &mut file_suppressions);
        } else if let Some(rules) = match_directive(text, "rigor:disable") {
            absorb_tokens(rules, line_suppressions.entry(*line).or_default());
        }
    }

    (line_suppressions, file_suppressions)
}

/// Find `#` `<ws>*` `<keyword>` `<ws>+` in `text` and return the rule-token tail
/// (everything after the keyword's trailing whitespace). Hand-rolled equivalent
/// of the reference's `/#\s*<keyword>\s+(?<rules>[\w.,\s-]+)/` (the `regex` crate
/// is a cached dep, but the patterns are simple enough to scan directly and avoid
/// pulling it into this crate). Returns `None` when the directive is absent or
/// has no whitespace-separated tail.
///
/// For the `rigor:disable` keyword the caller has already tried `disable-file`
/// first, which is how the reference's `(?!-file)` lookahead is honored: a
/// `disable-file` comment matches the `-file` branch and never reaches here.
fn match_directive<'a>(text: &'a str, keyword: &str) -> Option<&'a str> {
    let hash = text.find('#')?;
    let mut rest = &text[hash + 1..];
    // `#\s*`
    rest = rest.trim_start_matches([' ', '\t']);
    let after_kw = rest.strip_prefix(keyword)?;
    // `\s+` — at least one whitespace must follow the keyword.
    let trimmed = after_kw.trim_start_matches([' ', '\t']);
    if trimmed.len() == after_kw.len() {
        return None;
    }
    Some(trimmed)
}

/// Split the rule-token tail on whitespace/commas and absorb each token,
/// matching the reference's `raw.split(/[\s,]+/)`. The reference's `[\w.,\s-]+`
/// capture stops at the first character outside that class; tokens here are split
/// on the same delimiters, and any token is absorbed verbatim, so a trailing
/// non-rule word is simply an unknown token (a no-op).
fn absorb_tokens(tail: &str, target: &mut SuppressSet) {
    for token in tail.split([' ', '\t', ',']) {
        if !token.is_empty() {
            target.absorb_token(token);
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use rigor_parse::{lower, parse};

    fn run(src: &[u8]) -> Vec<Diagnostic> {
        let ast = lower(&parse(src));
        let mut interner = Interner::new();
        let index = CoreIndex::new();
        analyze(&ast, &mut interner, &index)
    }

    #[test]
    fn flags_typo_method_on_string_literal() {
        let src = b"s = \"Hello\"\ns.lenght\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1);
        let d = &diags[0];
        assert_eq!(d.rule_id, CALL_UNDEFINED_METHOD);
        assert_eq!(d.message, "undefined method `lenght' for \"Hello\"");
        // Severity must be Error for undefined-method.
        assert_eq!(d.severity, Severity::Error);
        assert_eq!(d.source_family, "builtin");
        // receiver_type matches the reference's rendering: the literal value
        // `"Hello"` (with surrounding double quotes), not the bare class name.
        assert_eq!(d.receiver_type.as_deref(), Some("\"Hello\""));
        assert_eq!(d.method_name.as_deref(), Some("lenght"));
        // The span must cover exactly `lenght`.
        assert_eq!(&src[d.start_offset..d.end_offset], b"lenght");
    }

    #[test]
    fn known_method_is_silent() {
        let diags = run(b"s = \"Hello\"\ns.length\n");
        assert!(diags.is_empty(), "expected no diagnostics, got {diags:?}");
    }

    #[test]
    fn dynamic_receiver_is_silent() {
        // `x` is never assigned => Dynamic[top] => never guess.
        let diags = run(b"x.foo\n");
        assert!(diags.is_empty(), "expected no diagnostics, got {diags:?}");
    }

    #[test]
    fn flags_wrong_arity_on_string_include() {
        // `String#include?` is arity (1, 1); two args is wrong-arity.
        let src = b"s = \"x\"\ns.include?(\"a\", \"b\")\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "expected one diagnostic, got {diags:?}");
        let d = &diags[0];
        assert_eq!(d.rule_id, CALL_WRONG_ARITY);
        assert_eq!(d.severity, Severity::Error);
        assert_eq!(d.source_family, "builtin");
        assert_eq!(d.receiver_type.as_deref(), Some("String"));
        assert_eq!(d.method_name.as_deref(), Some("include?"));
        assert_eq!(
            d.message,
            "wrong number of arguments to `include?' on String (given 2, expected 1)"
        );
        // Anchored on the method-name token `include?`.
        assert_eq!(&src[d.start_offset..d.end_offset], b"include?");
    }

    #[test]
    fn wrong_arity_renders_range_for_gsub() {
        // `String#gsub` is arity (1, 2); three args -> `expected 1..2`.
        let src = b"s = \"x\"\ns.gsub(\"a\", \"b\", \"c\")\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "expected one diagnostic, got {diags:?}");
        let d = &diags[0];
        assert_eq!(d.rule_id, CALL_WRONG_ARITY);
        assert_eq!(d.severity, Severity::Error);
        assert_eq!(d.receiver_type.as_deref(), Some("String"));
        assert_eq!(d.method_name.as_deref(), Some("gsub"));
        assert_eq!(
            d.message,
            "wrong number of arguments to `gsub' on String (given 3, expected 1..2)"
        );
    }

    #[test]
    fn correct_arity_is_silent() {
        // 1-arg include?, 1-arg and 2-arg gsub are all within envelope.
        assert!(run(b"s = \"x\"\ns.include?(\"a\")\n").is_empty());
        assert!(run(b"s = \"x\"\ns.gsub(\"a\")\n").is_empty());
        assert!(run(b"s = \"x\"\ns.gsub(\"a\", \"b\")\n").is_empty());
    }

    #[test]
    fn nil_literal_receiver_is_undefined_method() {
        // `x = nil; x.upcase` — receiver types to Constant[Nil]; the reference
        // routes a definitely-nil receiver to `call.undefined-method`, not
        // `possible-nil-receiver`. We match that.
        let src = b"x = nil\nx.upcase\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "expected one diagnostic, got {diags:?}");
        let d = &diags[0];
        assert_eq!(d.rule_id, CALL_UNDEFINED_METHOD);
        assert_eq!(d.severity, Severity::Error);
        assert_eq!(d.message, "undefined method `upcase' for nil");
        assert_eq!(&src[d.start_offset..d.end_offset], b"upcase");
    }

    #[test]
    fn no_false_positives_on_valid_code() {
        // A spread of valid calls across modeled classes must stay silent —
        // no arity, undefined-method, or nil diagnostics.
        assert!(run(b"s = \"x\"\ns.upcase\n").is_empty());
        assert!(run(b"n = 1\nn.abs\n").is_empty());
        assert!(run(b"s = \"hi\"\ns.gsub(\"a\", \"b\")\n").is_empty());
        // Dynamic receiver with any arity stays silent (never guess).
        assert!(run(b"x.foo(1, 2, 3)\n").is_empty());
        // A nullary call in its valid form stays silent.
        assert!(run(b"s = \"x\"\ns.chars\n").is_empty());
    }

    #[test]
    fn variadic_arity_method_does_not_fire() {
        // `String#concat` is variadic (`(*string | Integer) -> self`), so its
        // arity envelope has no upper bound => wrong-arity must NOT fire no
        // matter how many positional args are passed. (Real RBS now models a
        // concrete envelope for nearly every method; a variadic one is the case
        // where many args are still legal.)
        let diags = run(b"s = \"x\"\ns.concat(\"a\", \"b\")\n");
        assert!(diags.is_empty(), "expected no diagnostics, got {diags:?}");
    }

    #[test]
    fn block_bearing_call_is_not_witnessed() {
        // `{...}.select { block }.keys` — `select` with a block returns a Hash
        // (`.keys` is valid), and `select` with a block takes 0 positional args
        // (no wrong-arity). Block-form RETURN typing is now modeled, but a VALID
        // chained call on the (correct) block result must still stay silent.
        let diags = run(b"h = {a: 1}\nx = h.select { |k, v| v > 0 }.keys\n");
        assert!(diags.is_empty(), "block-call chain must be silent, got {diags:?}");
        // The same chain without the witnessing chain still silent on the block call.
        let diags2 = run(b"[1, 2].each_with_index { |e, i| e }\n");
        assert!(diags2.is_empty(), "expected no diagnostics, got {diags2:?}");
        // The exact reported FP shape (gitlab-foss authorize_granular_scopes_service.rb:102):
        // a hash-literal-shorthand receiver chained DIRECTLY into `.select { }.keys`.
        // Two FPs must NOT fire: (a) wrong-arity on `select` (block ⇒ 0 positional
        // args, but the no-block envelope is 1..N — arity stays silent on block
        // calls), and (b) undefined-method `keys` on the block result (the block
        // form returns Hash, on which `keys` is valid). The reference is silent on
        // this whole line; rigor-rs must be too.
        let diags3 = run(
            b"def f(token, boundaries, permissions)\n{ token:, boundaries:, permissions: }.select { |_, value| value.nil? }.keys\nend\n",
        );
        assert!(diags3.is_empty(), "literal-receiver block chain must be silent, got {diags3:?}");
    }

    #[test]
    fn block_call_result_typo_is_witnessed() {
        // RECOVERED coverage (CURRENT_WORK §4): the block-form RETURN is now
        // RBS-modeled, so a typo on the CHAINED result is witnessed again,
        // matching the reference. Guarded on the real RBS tree (under the stub
        // fallback block returns are unmodeled ⇒ silent ⇒ no diagnostic to find).
        let idx = CoreIndex::new();
        if !idx.knows_class("Enumerable") || !idx.class_has_method("Array", "map") {
            return;
        }
        // `arr.map { }.frist` -> map block form returns Array; `.frist` undefined.
        let diags = run(b"arr = [1, 2, 3]\narr.map { |n| n + 1 }.frist\n");
        assert_eq!(diags.len(), 1, "expected one undefined-method, got {diags:?}");
        assert_eq!(diags[0].rule_id, "call.undefined-method");
        assert_eq!(diags[0].method_name.as_deref(), Some("frist"));

        // `arr.select { }.frist` -> Array; `.frist` undefined.
        let diags = run(b"arr = [1, 2, 3]\narr.select { |n| n > 1 }.frist\n");
        assert_eq!(diags.len(), 1, "expected one undefined-method, got {diags:?}");
        assert_eq!(diags[0].method_name.as_deref(), Some("frist"));

        // `arr.each { }.frist` -> `each` returns self (Array); `.frist` undefined.
        let diags = run(b"arr = [1, 2, 3]\narr.each { |n| n }.frist\n");
        assert_eq!(diags.len(), 1, "expected one undefined-method, got {diags:?}");
        assert_eq!(diags[0].method_name.as_deref(), Some("frist"));

        // `s.tap { }.lenght` -> `tap` returns self (String); `.lenght` undefined.
        let diags = run(b"s = \"hello\"\ns.tap { |x| x }.lenght\n");
        assert_eq!(diags.len(), 1, "expected one undefined-method, got {diags:?}");
        assert_eq!(diags[0].method_name.as_deref(), Some("lenght"));
    }

    #[test]
    fn in_source_return_chain_typo_is_witnessed() {
        // ADR-0023 tier-4b: `user.full_name.lenght` where `def full_name;
        // "#{a} #{b}"; end` infers full_name : String, so `.lenght` on the
        // String result is witnessed against the real String RBS.
        let src = b"class User\n  def full_name\n    \"#{first} #{last}\"\n  end\nend\nuser = User.new\nuser.full_name.lenght\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "expected one undefined-method, got {diags:?}");
        assert_eq!(diags[0].rule_id, CALL_UNDEFINED_METHOD);
        assert_eq!(diags[0].method_name.as_deref(), Some("lenght"));
        assert_eq!(diags[0].receiver_type.as_deref(), Some("String"));
    }

    #[test]
    fn in_source_return_chain_valid_call_stays_silent() {
        // The other side: a VALID method on the inferred core return must NOT
        // fire — `full_name : String`, and `.length` is valid on String.
        let src = b"class User\n  def full_name\n    \"#{first} #{last}\"\n  end\nend\nuser = User.new\nuser.full_name.length\n";
        let diags = run(src);
        assert!(diags.is_empty(), "valid String#length on the inferred return must be silent, got {diags:?}");
    }

    #[test]
    fn in_source_param_dependent_return_is_silent() {
        // A param-dependent method types Dynamic ⇒ no return entry ⇒ the chained
        // call stays silent (the zero-FP keystone for tier-4b).
        let src = b"class C\n  def echo(x)\n    x\n  end\nend\nc = C.new\nc.echo(\"a\").lenght\n";
        let diags = run(src);
        assert!(diags.is_empty(), "param-dependent return must not be witnessed, got {diags:?}");
    }

    #[test]
    fn block_call_result_valid_call_stays_silent() {
        // The other side of the recovery: a VALID method on the (correctly
        // modeled) block result must NOT fire — `Hash#select { }` returns Hash,
        // so `.keys` is valid (the FP class the placeholder originally guarded).
        let idx = CoreIndex::new();
        if !idx.knows_class("Enumerable") || !idx.class_has_method("Array", "map") {
            return;
        }
        // `h.select { }.keys` -> Hash#keys valid -> silent.
        let diags = run(b"h = { a: 1 }\nh.select { |k, v| v > 0 }.keys\n");
        assert!(diags.is_empty(), "Hash#select block result is Hash; .keys valid, got {diags:?}");
        // `h.reject { }.keys` -> Hash#reject block form returns Hash -> .keys valid.
        let diags = run(b"h = { a: 1 }\nh.reject { |k, v| v > 0 }.keys\n");
        assert!(diags.is_empty(), "Hash#reject block result is Hash; .keys valid, got {diags:?}");
        // `arr.map { }.first` -> Array#first valid -> silent.
        let diags = run(b"arr = [1, 2, 3]\narr.map { |n| n }.first\n");
        assert!(diags.is_empty(), "Array#map block result is Array; .first valid, got {diags:?}");
    }

    // --- in-source / non-core `.new` instances: reference leniency -----------
    //
    // The reference does NOT witness `undefined-method` on a project-defined
    // class instance, nor on a non-core `X.new` instance (Pathname/Set/Struct):
    // it gates on `rbs_class_known?` (check_rules.rb:556) and treats a miss there
    // leniently (ADR-0023 tier-4). rigor-rs mirrors that — these receivers are
    // typed (for chaining) but never witnessed. Every case below MUST be silent.

    #[test]
    fn in_source_instance_typo_is_silent_lenient() {
        // `class Point; def x; end; end; p = Point.new; p.y` — `y` is undefined on
        // Point, but Point is a project class (not RBS-known) ⇒ the reference stays
        // silent (leniency: Ruby defines methods dynamically). So must rigor-rs.
        let diags = run(b"class Point\n  def x\n  end\nend\np = Point.new\np.y\n");
        assert!(diags.is_empty(), "project-class miss must be silent, got {diags:?}");
    }

    #[test]
    fn defined_in_source_method_is_silent() {
        // `p.x` where Point defines `x` ⇒ no diagnostic (and silent regardless).
        let diags = run(b"class Point\n  def x\n  end\nend\np = Point.new\np.x\n");
        assert!(diags.is_empty(), "expected no diagnostics, got {diags:?}");
    }

    #[test]
    fn inherited_object_method_on_source_instance_is_silent() {
        // `p.frozen?` — inherited from Object via the source class's implicit
        // super; must not be a false positive.
        let diags = run(b"class Point\n  def x\n  end\nend\np = Point.new\np.frozen?\n");
        assert!(diags.is_empty(), "expected no diagnostics, got {diags:?}");
    }

    #[test]
    fn unknown_superclass_keeps_source_instance_silent() {
        // `class User < ApplicationRecord; end; u = User.new; u.anything` — silent
        // both because the super is unknown AND because a project class is never
        // witnessed. The zero-FP keystone for Rails models.
        let diags = run(
            b"class User < ApplicationRecord\nend\nu = User.new\nu.totally_made_up_xyz\n",
        );
        assert!(diags.is_empty(), "expected no diagnostics, got {diags:?}");
    }

    #[test]
    fn source_subclass_typo_is_silent_lenient() {
        // `class Animal; def speak; end; end; class Dog < Animal; end` — neither
        // an inherited method nor a typo is witnessed on the project class `Dog`
        // (reference leniency), even though the chain Dog->Animal->Object is known.
        let ok = run(b"class Animal\n  def speak\n  end\nend\nclass Dog < Animal\nend\nd = Dog.new\nd.speak\n");
        assert!(ok.is_empty(), "inherited method must be silent, got {ok:?}");
        let bad = run(b"class Animal\n  def speak\n  end\nend\nclass Dog < Animal\nend\nd = Dog.new\nd.fly\n");
        assert!(bad.is_empty(), "project-class typo must be silent (leniency), got {bad:?}");
    }

    #[test]
    fn reopened_source_class_is_silent_lenient() {
        // A project class is never witnessed, reopened or not — including a typo.
        assert!(run(b"class C\n  def a\n  end\nend\nclass C\n  def b\n  end\nend\nc = C.new\nc.a\n").is_empty());
        let typo = run(b"class C\n  def a\n  end\nend\nclass C\n  def b\n  end\nend\nc = C.new\nc.zzz\n");
        assert!(typo.is_empty(), "project-class typo must be silent (leniency), got {typo:?}");
    }

    #[test]
    fn non_core_rbs_new_instance_typo_is_silent_lenient() {
        // `Pathname.new("a").nonexist` — Pathname is RBS-known but NOT a core
        // class round-tripped by id, so it resolves only through the registry
        // surface. The reference is silent on `Pathname.new.typo` (leniency on a
        // non-core `.new` instance); rigor-rs mirrors that — always silent.
        let diags = run(b"p = Pathname.new(\"a\")\np.nonexist\n");
        assert!(diags.is_empty(), "non-core .new instance miss must be silent, got {diags:?}");
    }

    #[test]
    fn metaclass_constructor_chained_new_is_silent() {
        // `Struct.new(:a, :b).new(1, 2)` — `Struct.new` returns a CLASS, not a
        // Struct instance; the chained `.new` must not be witnessed absent.
        let diags = run(b"Struct.new(:a, :b).new(1, 2)\n");
        assert!(diags.is_empty(), "Struct.new(...).new must be silent, got {diags:?}");
    }

    #[test]
    fn core_new_instance_typo_still_flags() {
        // The core `.new` path is still witnessed (matches the reference, which
        // flags `Array.new.bogus`): `Array` IS a core class round-tripped by id.
        let idx = CoreIndex::new();
        let diags = run(b"Array.new.bogus_xyz\n");
        if idx.knows_class("Array") {
            assert_eq!(diags.len(), 1, "expected core .new typo flagged, got {diags:?}");
            assert_eq!(diags[0].rule_id, CALL_UNDEFINED_METHOD);
            assert_eq!(diags[0].method_name.as_deref(), Some("bogus_xyz"));
        }
    }

    #[test]
    fn real_rbs_method_on_rbs_instance_is_silent() {
        // `Pathname.new("a").basename` — a real method, never a false positive.
        let diags = run(b"p = Pathname.new(\"a\")\np.basename\n");
        assert!(diags.is_empty(), "expected no diagnostics, got {diags:?}");
    }

    #[test]
    fn dynamic_unknown_constant_new_is_silent() {
        // `Widget.new.foo` where Widget is unknown ⇒ Dynamic ⇒ silent.
        let diags = run(b"w = Widget.new\nw.foo\n");
        assert!(diags.is_empty(), "expected no diagnostics, got {diags:?}");
    }

    // --- singleton (class-method) witnessing on bare constants ---------------
    //
    // A bare top-level RBS constant (`Time`, `Array`) types to `Singleton(C)`;
    // a class-method typo on it is witnessed (`Time.current`), while real class
    // methods, instance-only names, `.new`, and project-class collisions stay
    // silent. All guarded on real RBS being loaded (stub ⇒ assert silent).

    #[test]
    fn time_current_flags_singleton() {
        let idx = CoreIndex::new();
        let diags = run(b"Time.current\n");
        if idx.knows_class("Time") {
            assert_eq!(diags.len(), 1, "expected one diagnostic, got {diags:?}");
            let d = &diags[0];
            assert_eq!(d.rule_id, CALL_UNDEFINED_METHOD);
            assert_eq!(d.severity, Severity::Error);
            assert_eq!(d.message, "undefined method `current' for singleton(Time)");
            assert_eq!(d.receiver_type.as_deref(), Some("singleton(Time)"));
            assert_eq!(d.method_name.as_deref(), Some("current"));
        } else {
            assert!(diags.is_empty(), "stub fallback must be silent, got {diags:?}");
        }
    }

    #[test]
    fn time_real_class_methods_and_new_are_silent() {
        // `Time.now` / `Time.name` are real class methods; `Time.new` constructs
        // an instance (intercepted before singleton typing). All silent.
        assert!(run(b"Time.now\n").is_empty(), "Time.now must be silent");
        assert!(run(b"Time.name\n").is_empty(), "Time.name must be silent");
        assert!(run(b"Time.new\n").is_empty(), "Time.new must be silent");
    }

    #[test]
    fn array_wrap_flags_singleton_but_new_is_silent() {
        let idx = CoreIndex::new();
        // `Array.wrap` is an ActiveSupport extension, not core ⇒ flagged absent.
        let diags = run(b"Array.wrap(x)\n");
        if idx.knows_class("Array") {
            assert_eq!(diags.len(), 1, "expected Array.wrap flagged, got {diags:?}");
            assert_eq!(diags[0].message, "undefined method `wrap' for singleton(Array)");
            assert_eq!(diags[0].receiver_type.as_deref(), Some("singleton(Array)"));
        } else {
            assert!(diags.is_empty(), "stub fallback must be silent, got {diags:?}");
        }
        // `Array.new` constructs an instance ⇒ silent (not singleton-typed).
        assert!(run(b"Array.new\n").is_empty(), "Array.new must be silent");
    }

    #[test]
    fn project_class_collision_is_silent() {
        // A file that DEFINES `class Group` and also calls `Group.where(1)`: even
        // though `Group` may be a top-level RBS class, the project defines it, so
        // the gate refuses to singleton-type it ⇒ silent (cross-file zero-FP).
        let diags = run(b"class Group\nend\nGroup.where(1)\n");
        assert!(diags.is_empty(), "project-class collision must be silent, got {diags:?}");
    }

    #[test]
    fn secure_random_hex_is_silent_extend_surface() {
        // `SecureRandom.hex` — its class methods come via an `extend`ed module, so
        // the class-method surface is incomplete ⇒ conservative ⇒ silent.
        let diags = run(b"SecureRandom.hex\n");
        assert!(diags.is_empty(), "SecureRandom.hex must be silent, got {diags:?}");
    }

    #[test]
    fn catalog_entries_are_correct() {
        let entry = catalog(CALL_UNDEFINED_METHOD).expect("catalog entry must exist");
        assert_eq!(entry.default_severity, Severity::Error);
        assert_eq!(entry.evidence_tier, "high");
        assert!(entry.documentation_url.contains("call-undefined-method"));

        let entry = catalog(CALL_WRONG_ARITY).expect("catalog entry must exist");
        assert_eq!(entry.default_severity, Severity::Error);
        assert_eq!(entry.evidence_tier, "high");

        let entry = catalog(CALL_POSSIBLE_NIL_RECEIVER).expect("catalog entry must exist");
        assert_eq!(entry.default_severity, Severity::Warning);
        assert_eq!(entry.evidence_tier, "medium");

        assert!(catalog("unknown.rule").is_none());
    }

    // -- in-source suppression --------------------------------------------

    fn diag(rule: &'static str) -> Diagnostic {
        Diagnostic {
            rule_id: rule,
            start_offset: 0,
            end_offset: 0,
            message: String::new(),
            severity: Severity::Error,
            source_family: "builtin",
            receiver_type: None,
            method_name: None,
        }
    }

    fn surviving_rules(
        diags: Vec<(usize, Diagnostic)>,
        comments: &[(usize, String)],
    ) -> Vec<(usize, &'static str)> {
        filter_suppressed(diags, comments)
            .into_iter()
            .map(|(line, d)| (line, d.rule_id))
            .collect()
    }

    #[test]
    fn line_disable_drops_only_that_lines_rule() {
        let diags = vec![
            (2, diag(CALL_UNDEFINED_METHOD)),
            (4, diag(CALL_UNDEFINED_METHOD)),
        ];
        let comments = vec![(4, "# rigor:disable call.undefined-method".to_string())];
        // Only the L4 diagnostic is suppressed; L2 survives.
        assert_eq!(surviving_rules(diags, &comments), vec![(2, CALL_UNDEFINED_METHOD)]);
    }

    #[test]
    fn line_disable_all_drops_every_rule_on_that_line() {
        let diags = vec![
            (3, diag(CALL_UNDEFINED_METHOD)),
            (3, diag(CALL_WRONG_ARITY)),
            (5, diag(CALL_WRONG_ARITY)),
        ];
        let comments = vec![(3, "# rigor:disable all".to_string())];
        assert_eq!(surviving_rules(diags, &comments), vec![(5, CALL_WRONG_ARITY)]);
    }

    #[test]
    fn disable_file_drops_rule_on_every_line() {
        let diags = vec![
            (2, diag(CALL_UNDEFINED_METHOD)),
            (9, diag(CALL_UNDEFINED_METHOD)),
            (9, diag(CALL_WRONG_ARITY)),
        ];
        // The directive sits on line 1 but scopes the whole file.
        let comments = vec![(1, "# rigor:disable-file undefined-method".to_string())];
        assert_eq!(surviving_rules(diags, &comments), vec![(9, CALL_WRONG_ARITY)]);
    }

    #[test]
    fn disable_file_all_drops_everything() {
        let diags = vec![
            (2, diag(CALL_UNDEFINED_METHOD)),
            (4, diag(CALL_WRONG_ARITY)),
            (6, diag(CALL_POSSIBLE_NIL_RECEIVER)),
        ];
        let comments = vec![(1, "# rigor:disable-file all".to_string())];
        assert!(filter_suppressed(diags, &comments).is_empty());
    }

    #[test]
    fn family_token_call_expands_to_all_call_rules() {
        let diags = vec![
            (2, diag(CALL_UNDEFINED_METHOD)),
            (2, diag(CALL_WRONG_ARITY)),
            (2, diag(CALL_POSSIBLE_NIL_RECEIVER)),
        ];
        let comments = vec![(2, "# rigor:disable call".to_string())];
        assert!(filter_suppressed(diags, &comments).is_empty());
    }

    #[test]
    fn legacy_alias_resolves_to_canonical_id() {
        let diags = vec![(4, diag(CALL_UNDEFINED_METHOD))];
        let comments = vec![(4, "# rigor:disable undefined-method".to_string())];
        assert!(filter_suppressed(diags, &comments).is_empty());
    }

    #[test]
    fn comma_and_whitespace_separated_tokens() {
        let diags = vec![
            (2, diag(CALL_UNDEFINED_METHOD)),
            (2, diag(CALL_WRONG_ARITY)),
        ];
        let comments = vec![(2, "# rigor:disable undefined-method, wrong-arity".to_string())];
        assert!(filter_suppressed(diags, &comments).is_empty());
    }

    #[test]
    fn unrelated_rule_or_line_is_not_suppressed() {
        // A disable for a DIFFERENT rule on the same line must not drop it.
        let same_line = filter_suppressed(
            vec![(4, diag(CALL_UNDEFINED_METHOD))],
            &[(4, "# rigor:disable wrong-arity".to_string())],
        );
        assert_eq!(same_line.len(), 1);

        // A disable on a DIFFERENT line must not drop it.
        let other_line = filter_suppressed(
            vec![(4, diag(CALL_UNDEFINED_METHOD))],
            &[(7, "# rigor:disable undefined-method".to_string())],
        );
        assert_eq!(other_line.len(), 1);
    }

    #[test]
    fn disable_file_negative_lookahead_not_read_as_line_disable() {
        // `disable-file` must NOT also register as a line-level `disable` for the
        // comment's own line (reference `(?!-file)`).
        let line_set =
            parse_suppression_comments(&[(3, "# rigor:disable-file undefined-method".to_string())]);
        assert!(line_set.0.get(&3).is_none());
        assert!(line_set.1.suppresses(CALL_UNDEFINED_METHOD));
    }

    #[test]
    fn internal_error_is_never_suppressed() {
        let diags = vec![(2, diag(INTERNAL_ERROR_RULE))];
        let comments = vec![(2, "# rigor:disable all".to_string())];
        // Even `disable all` cannot silence an internal-error diagnostic.
        assert_eq!(filter_suppressed(diags, &comments).len(), 1);
    }

    #[test]
    fn suppress_set_from_tokens_legacy_alias() {
        // The public config helper expands the legacy alias to its canonical id.
        let set = SuppressSet::from_tokens(&["undefined-method"]);
        assert!(set.suppresses(CALL_UNDEFINED_METHOD));
        assert!(!set.suppresses(CALL_WRONG_ARITY));
    }

    #[test]
    fn suppress_set_from_tokens_call_family_and_canonical() {
        // `call` family wildcard expands to every implemented call.* id.
        let set = SuppressSet::from_tokens(&["call"]);
        assert!(set.suppresses(CALL_UNDEFINED_METHOD));
        assert!(set.suppresses(CALL_WRONG_ARITY));
        assert!(set.suppresses(CALL_POSSIBLE_NIL_RECEIVER));
        // A canonical id passes through to itself.
        let set = SuppressSet::from_tokens(&[CALL_WRONG_ARITY]);
        assert!(set.suppresses(CALL_WRONG_ARITY));
        assert!(!set.suppresses(CALL_UNDEFINED_METHOD));
    }

    #[test]
    fn suppress_set_from_tokens_never_matches_internal_error() {
        // Neither `all` nor an explicit `internal-error` token may match the
        // internal-error sentinel — it stays reportable through config too.
        assert!(!SuppressSet::from_tokens(&["all"]).suppresses(INTERNAL_ERROR_RULE));
        assert!(!SuppressSet::from_tokens(&["internal-error"]).suppresses(INTERNAL_ERROR_RULE));
    }

    #[test]
    fn suppress_set_from_tokens_empty_and_unknown_are_inert() {
        let empty: [&str; 0] = [];
        assert!(!SuppressSet::from_tokens(&empty).suppresses(CALL_UNDEFINED_METHOD));
        // An unknown token matches no real diagnostic.
        let set = SuppressSet::from_tokens(&["not-a-real-rule"]);
        assert!(!set.suppresses(CALL_UNDEFINED_METHOD));
        assert!(!set.suppresses(CALL_WRONG_ARITY));
    }
}
