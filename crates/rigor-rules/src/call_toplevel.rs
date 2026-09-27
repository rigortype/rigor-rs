//! `call.unresolved-toplevel` (ref ADR-34): a toplevel implicit-self call
//! whose name is unresolved, and the class-body regions (meta-new and
//! receiver-eval blocks) that are not toplevel.

use rigor_index::CoreIndex;
use rigor_parse::{LoweredAst, Node};

use crate::{catalog, Diagnostic, Severity, CALL_UNRESOLVED_TOPLEVEL};

/// Toplevel `Kernel` methods that the RUNTIME Ruby injects but the vendored
/// RBS does not model, so `class_has_method("Object", …)` misses them. The
/// reference resolves these via runtime reflection on `Object`; rigor-rs mirrors
/// that result with this small, FP-safe allowlist. `gem` is RubyGems' `Kernel#gem`
/// (the only core-only case the corpus FP audit surfaced). Extend as real signal
/// appears — never a false positive, only a missed witness if wrong.
const RUNTIME_KERNEL_TOPLEVEL: &[&str] = &["gem"];

/// Emit `call.unresolved-toplevel` for every toplevel implicit-self call whose
/// name is unresolved. Zero-FP gate (fires ⊆ the reference): suppress on the
/// `Object` RBS surface (`class_has_method("Object", …)` — witnessed-absent only
/// when Object's full core chain is loaded, so a miss there stays silent) AND on
/// same-file toplevel `def` names AND on in-source `Object`/`Kernel`/`BasicObject`
/// reopen methods (the reference's `source_declared_method?` path). `pre_eval:`
/// monkey-patches are not modeled (rigor-rs has no `pre_eval`), so a project that
/// injects toplevel methods that way would see a firing — the reference routes the
/// same case to `pre_eval:` in the message; on the config-less corpus/harness the
/// two agree exactly.
pub(crate) fn unresolved_toplevel_diagnostics(
    ast: &LoweredAst,
    index: &CoreIndex,
    source: &rigor_infer::SourceIndex,
    out: &mut Vec<Diagnostic>,
) {
    // Spans of every CLASS/MODULE body — the NON-toplevel regions. `def` spans are
    // deliberately EXCLUDED: the reference's `scope.toplevel?` means "outside any
    // class/module body", so a TOPLEVEL `def`'s body is still toplevel (the rule
    // fires on an unresolved implicit-self call there) — only a `def` nested in a
    // class/module (a method) is non-toplevel, and its calls fall inside the
    // enclosing class/module span.
    let mut scope_spans: Vec<rigor_parse::Span> = ast
        .iter()
        .filter_map(|(_, n)| match n {
            // A `class << X` singleton-class body is a CLASS scope too — the
            // reference stays silent on implicit-self calls inside it (FP audit:
            // net-ssh/algorithms fired here). A method `def` (name-less or not) is
            // NOT a class scope: a toplevel `def` body still fires.
            Node::ClassDef { .. } | Node::ModuleDef { .. } => Some(n.span()),
            Node::Definition { is_singleton_class: true, .. } => Some(n.span()),
            _ => None,
        })
        .collect();
    scope_spans.extend(meta_new_block_body_spans(ast));
    scope_spans.extend(receiver_eval_block_spans(ast));

    for (_, n) in ast.iter() {
        if let Node::Call { receiver: None, method, message_span, .. } = n {
            // Not at toplevel (nested in a class/module) ⇒ silent (ADR-24).
            if span_contained_in_any(n.span(), &scope_spans) {
                continue;
            }
            // Resolves against a PROJECT-WIDE toplevel `def` / Object-reopen ⇒
            // silent. Cross-file (not just same-file) matches the reference's
            // project-mode resolution — a `def` in a required file resolves the
            // call — which is what keeps the multi-file corpus zero-FP.
            if source.is_toplevel_def(Some(ast.file_key()), method) {
                continue;
            }
            // Present on the Object/Kernel instance surface ⇒ silent. (`false`
            // is witnessed-absent only when Object's whole core chain is loaded;
            // an incomplete chain returns `true` ⇒ we stay silent — never an FP.)
            if index.class_has_method("Object", method) {
                continue;
            }
            // Runtime-injected Kernel toplevel methods the vendored RBS doesn't
            // model, but the live Ruby does — so the reference (which resolves via
            // runtime reflection on `Object`, `check_rules.rb`) stays silent. `gem`
            // (RubyGems' `Kernel#gem`) is the core-only case the net-ssh FP audit
            // surfaced. FP-safe: this only ever silences.
            if RUNTIME_KERNEL_TOPLEVEL.contains(&method.as_str()) {
                continue;
            }
            let severity = catalog(CALL_UNRESOLVED_TOPLEVEL)
                .map(|e| e.default_severity)
                .unwrap_or(Severity::Warning);
            out.push(Diagnostic {
                rule_id: CALL_UNRESOLVED_TOPLEVEL,
                start_offset: message_span.0,
                end_offset: message_span.1,
                message: format!(
                    "unresolved toplevel call to `{method}`. If a project file defines \
                     `{method}` via a toplevel `def` or a monkey-patch on Object/Kernel, list \
                     that file in `.rigor.yml`'s `pre_eval:` (ADR-17) so the analyzer sees it."
                ),
                severity,
                source_family: "builtin",
                receiver_type: None,
                method_name: Some(method.clone()),
            });
        }
    }
}

/// The class-creating meta calls whose literal block body is a CLASS BODY at
/// runtime — the reference's `AnonymousMetaClass::META_NEW_SELECTORS`
/// (upstream #319 / `189c498b`, pin `v0.3.4`). `Struct.new` / `Data.define` are
/// here for the same reason `Class.new` / `Module.new` are: the block is
/// `class_eval`'d on the generated class, so `self` inside it is that class.
const META_NEW_SELECTORS: &[(&str, &str)] = &[
    ("Class", "new"),
    ("Module", "new"),
    ("Struct", "new"),
    ("Data", "define"),
];

/// The block-body regions of every `Class.new do … end` / `Module.new { … }` /
/// `Struct.new(…) do … end` / `Data.define(…) do … end` in the file — NON-toplevel
/// scopes for `call.unresolved-toplevel`.
///
/// Ruby evaluates such a block with `self` bound to the freshly created class, so
/// `attr_reader` there is a class-level macro and not an unresolved toplevel call.
/// The reference learned this away from constant-write position at `v0.3.4`
/// (upstream #319): the body now enters under `Singleton[<anonymous>]`, and
/// `Scope#toplevel?` is `self_type.nil?`, so the rule can no longer fire inside
/// one. Before the fix it did, and rigor-rs matched; at the `v0.3.4` pin the same
/// output became **48 false positives** across the standing sweep (dependabot-core
/// `base_spec.rb` ×40, concurrent-ruby `erlang_actor_spec.rb` ×8 — RSpec's
/// `Class.new(described_class) do … end` / `Module.new do … end` idiom).
///
/// Only the BLOCK BODY is a class scope; the receiver and the arguments keep the
/// enclosing scope, so an implicit-self call in `Class.new(parent_of(x)) { … }`'s
/// arguments still fires. The returned span covers the body statements — a call
/// nested anywhere inside one of them is contained in it.
///
/// The identity half of the upstream change (a synthetic `#<Class:path:line:col>`
/// name, under which the body's `def`s / `attr_*` are registered and which
/// `class_new_lift` returns instead of `Singleton[Object]`) is NOT ported here:
/// it only ever makes the reference emit MORE, so it is coverage, not FP safety.
///
/// **The constant-write rvalue is included**, since upstream #590 / `b3d688f7`
/// (pin `v0.3.8`). It used to be carved out because the reference's own
/// `StatementEvaluator` had no `ConstantWriteNode` handler at all: the rvalue fell
/// to the pure-expression default, its block was never walked, and
/// `ScopeIndexer.propagate` handed every node inside the ENCLOSING scope — a nil
/// `self_type` at file top level, which is exactly what `Scope#toplevel?` keys on.
/// `b3d688f7` added the handler and routes the rvalue block through the same
/// `enter_meta_class_body` the #319 arm uses, so `Registry = Class.new do
/// attr_reader :entries end` is now silent on both sides. Measured at the
/// `v0.3.8` pin (`ffb456b0`): rows d1-d4, d8 and d10' of
/// `docs/notes/20260909-repin-v038-port-spec.md` are silent on the oracle.
///
/// Two spellings where the ORACLE still fires and rigor-rs is silent are coverage
/// gaps, not FP risk, and are deliberately not chased: `X = Class.new do … end
/// .freeze` and `X ||= Class.new do … end` — in both the constant's rvalue is not
/// the meta-new call itself, so upstream's `meta_new_constant_body_context`
/// declines while rigor-rs's span scan sees the inner `Class.new` block regardless.
/// `Outer::F = …` lowers to the recovery carrier (no owned `ConstantPathWrite`
/// variant) and is silent on BOTH sides (row d5).
fn meta_new_block_body_spans(ast: &LoweredAst) -> Vec<rigor_parse::Span> {
    let mut out = Vec::new();
    for (_, n) in ast.iter() {
        let Node::Call {
            receiver: Some(recv),
            method,
            block_body,
            ..
        } = n
        else {
            continue;
        };
        if block_body.is_empty() {
            continue;
        }
        // The receiver MUST be the bare constant (`Class`, or its `::`-rooted
        // spelling, which lowers to the same name) — a call through a variable
        // has no statically known identity, exactly as the reference requires.
        let Node::ConstantRead { name, .. } = ast.get(*recv) else {
            continue;
        };
        if !META_NEW_SELECTORS
            .iter()
            .any(|(konst, sel)| name == konst && method == sel)
        {
            continue;
        }
        let start = block_body.iter().map(|id| ast.get(*id).span().0).min();
        let end = block_body.iter().map(|id| ast.get(*id).span().1).max();
        if let (Some(start), Some(end)) = (start, end) {
            out.push((start, end));
        }
    }
    out
}

/// The reference's `RECEIVER_EVAL_CALL_NAMES` (`check_rules.rb`, upstream #1135
/// / `ee33407e`): the calls whose literal block Ruby evaluates with `self`
/// rebound to the receiver.
const RECEIVER_EVAL_CALL_NAMES: &[&str] = &[
    "class_eval",
    "module_eval",
    "class_exec",
    "module_exec",
    "instance_eval",
    "instance_exec",
];

/// The literal-block regions of every `class_eval` / `module_eval` /
/// `class_exec` / `module_exec` / `instance_eval` / `instance_exec` call in the
/// file — `call.unresolved-toplevel` declines any receiverless call inside one.
///
/// A faithful port of the reference's `receiver_eval_block_ranges` +
/// `call_inside_receiver_eval_ranges?` (upstream #1135, `ee33407e` / `f918c6f0`,
/// pin `e59b7b89`): an eval body is morally a class body, so ADR-34 stays silent
/// there — on a genuinely undefined name too. The reference keys on the call's
/// NAME alone and on offsets alone, so, unlike [`meta_new_block_body_spans`]:
///
/// * the receiver shape is irrelevant — a constant, a local, `self`, another
///   call, or NO receiver at all (a bare `class_eval do … end`) all qualify;
/// * the range is the whole `BlockNode` (`{ … }` and `do … end` alike,
///   parameters and delimiters included), so a call inside a `def` or a nested
///   block within the eval block is covered, and so is a heredoc body that sits
///   textually inside it;
/// * only a LITERAL block counts: the string form `class_eval("…")` and the
///   block-pass `class_eval(&blk)` carry no `BlockNode`, and the arguments keep
///   the enclosing scope — `Foo.class_eval(helper) do … end` still fires on
///   `helper`.
///
/// The measured motive: 9 standing-sweep false positives in rspec's
/// `minitest_integration.rb` (`Minitest::Test.class_eval do include
/// ::RSpec::Matchers … end`). Upstream's companion `fb781023` (moving eval-block
/// `def`s off the toplevel-def table) is deliberately NOT ported with it: alone
/// it only removes resolutions, i.e. would create firings.
fn receiver_eval_block_spans(ast: &LoweredAst) -> Vec<rigor_parse::Span> {
    ast.iter()
        .filter_map(|(_, n)| match n {
            Node::Call { method, block_span: Some(block), .. }
                if RECEIVER_EVAL_CALL_NAMES.contains(&method.as_str()) =>
            {
                Some(*block)
            }
            _ => None,
        })
        .collect()
}

/// Whether `span` is contained in ANY of `spans` (non-strict). Used to decide a
/// call is inside some def/class/module body (a call span never equals a scope
/// span, so no self-match).
fn span_contained_in_any(span: rigor_parse::Span, spans: &[rigor_parse::Span]) -> bool {
    spans.iter().any(|s| s.0 <= span.0 && span.1 <= s.1)
}
