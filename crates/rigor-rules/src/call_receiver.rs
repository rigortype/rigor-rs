//! The receiver rules: `call.undefined-method` (scalar, union,
//! class-narrowed and collection-shape receivers) and
//! `call.possible-nil-receiver`.

use rigor_index::CoreIndex;
use rigor_infer::Typer;
use rigor_parse::LoweredAst;
use rigor_types::{Interner, Scalar, Type};

use crate::{
    catalog, render_receiver, Diagnostic, Severity, CALL_POSSIBLE_NIL_RECEIVER,
    CALL_UNDEFINED_METHOD,
};

// ---------------------------------------------------------------------------
// Rule implementations
// ---------------------------------------------------------------------------

/// The reference's `METACLASS_ARMS` (`analysis/check_rules.rb`), verbatim: a value
/// typed `Class` or `Module` is SOME class or module object, and its singleton
/// methods cannot be read off the metaclass — `def self.included(base)` receives
/// the includer, so `base.class_attribute :main_menu` is a call on whatever
/// included the module.
const METACLASS_ARMS: &[&str] = &["Class", "Module"];

/// Whether `call.undefined-method` must DECLINE on a receiver whose resolved class
/// is `class_name`, because that class's method surface is not enumerable.
///
/// The port of upstream #742 / PR #743 (`23341a87`) — `unenumerable_receiver?`,
/// which is `METACLASS_ARMS ∪ unbounded_receiver_surface?` and runs on BOTH the
/// instance and the singleton side. rigor-rs has no `unbounded_receiver_surface?`
/// analogue at this seam (ADR-26 open classes and synthesized stubs are handled by
/// the conservative `class_has_method` completeness gate), so only the metaclass
/// half needs porting. Measured at pin `ffb456b0`: `Class.frobnicate` and
/// `Module.frobnicate` — SINGLETON reads of the two constants — are silent on the
/// oracle and were rigor-rs false positives (rows x3/x4 of
/// `docs/notes/20260909-repin-v038-rules-families.md`).
fn unenumerable_metaclass_receiver(class_name: &str) -> bool {
    METACLASS_ARMS.contains(&class_name.strip_prefix("::").unwrap_or(class_name))
}

/// Whether `call.undefined-method` must DECLINE on an INSTANCE-side receiver whose
/// resolved class is `class_name`.
///
/// The port of upstream #739 / PR #741 (`3636649f`) joined with the metaclass half
/// above. A value typed as a mixin MODULE is an instance of whatever class includes
/// the module, and that class contributes an arbitrary surface: in RBS a parameter
/// typed `Taggable` means "something whose class includes Taggable", not "something
/// whose methods are Taggable's". Nothing there can prove a method absent, so the
/// reference stopped retrying the lookup against `Object` and declines outright
/// (`module_mixin_receiver?` in `last_resort_surface_answers?`).
///
/// SCOPE, three ways, each measured rather than argued:
///
/// * INSTANCE side only for the module half. The reference's `module_mixin_receiver?`
///   tests `receiver_type.is_a?(Type::Nominal)`, so a SINGLETON module receiver keeps
///   firing — `Comparable.typo`, `Digest::Instance.typo` (rows c4/c5) — because a
///   namespace module's `module_function` / `def self.` surface is real and
///   enumerable. Declining every `Singleton` receiver would silence those and `String
///   .typo` (c11); that is the control an over-broad fix fails.
/// * `call.undefined-method` only. Upstream moved nothing else: `unenumerable_receiver?`
///   is called from the undefined-method diagnostic alone, and the arity rule reads the
///   narrower `unbounded_receiver_surface?`. Probed at the pin: `v.hexdigest(1, 2, 3)`
///   behind a `Digest::Instance` guard still reports `call.wrong-arity` on the oracle
///   (row x1) — rigor-rs is silent there for an unrelated, pre-existing reason.
/// * The QUALIFIED name, not the short key. `is_qualified_module` asks the isolated
///   registry exactly as the reference's `rbs_module?` asks `env.class_decls`; the
///   short-key map answers `false` for `Digest::Instance` and would leak a nested
///   module's moduleness onto an unrelated project class of the same leaf name.
///
/// The one shape upstream keys on SYNTAX rather than type — `mixin_self_class_receiver?`,
/// i.e. `v.class.typo` where `v` is mixin-typed — needs no port: rigor-rs is already
/// silent there on both engines (row c6), because `.class` on a Dynamic receiver
/// yields no witnessable carrier.
fn unenumerable_instance_receiver(index: &CoreIndex, class_name: &str) -> bool {
    unenumerable_metaclass_receiver(class_name) || index.is_qualified_module(class_name)
}

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
// too_many_arguments: a rule-check fn threading the full typing context (ast, receiver,
// span, env, typer, interner, index); bundling into a struct would obscure the call sites.
#[allow(clippy::too_many_arguments)]
pub(crate) fn check_call(
    ast: &LoweredAst,
    receiver: rigor_parse::NodeId,
    method: &str,
    message_span: (usize, usize),
    safe_nav: bool,
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
        // Upstream #742 — `Class.typo` / `Module.typo`. The reference's
        // `unenumerable_receiver?` runs before the surface lookup on BOTH sides,
        // so the two generic metaclasses decline here as well. The MODULE half of
        // the retraction deliberately does NOT reach this branch: a named module's
        // singleton surface is real and enumerable, and `Comparable.typo` /
        // `Digest::Instance.typo` keep firing (rows c4/c5).
        if unenumerable_metaclass_receiver(name) {
            return None;
        }
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

    // RBS-known class instance carried by a source-registry `Nominal` that
    // `class_name_of` (core-id only) will not resolve — recover the name from
    // the source registry and witness when the loaded RBS models the class
    // (`knows_class`): a project-`sig/` class (ADR-0033, `Widget.new` — project
    // sig is authoritative) AND a stdlib/core instance minted by a
    // declaration-driven path (a singleton RBS return — `Date.today`,
    // `Time.now` — or a C5 `Range` constant), which the reference witnesses
    // identically (probed: `Date.today.end_of_month`, `R.frobnicate` for a
    // Range constant both fire there). An in-source-only class (not in any
    // loaded RBS) stays lenient, and the stdlib `.new` leniency
    // (`Pathname.new("x").nope`) now lives in `type_dot_new`, which declines
    // the mint for a bundled non-core class, so no such Nominal reaches this
    // gate. `class_has_method` keeps its conservative completeness gate (an
    // incomplete ancestor chain ⇒ `true` ⇒ silent).
    // Gated on `knows_toplevel_class` (∪ project-sig), NOT `knows_class`: a
    // name the index knows only via a namespaced short-key registration
    // (`Instance` = some RBS `…::Instance`; the defect-2 set) can equally be a
    // PROJECT model's bare name (`Clusters::Instance` — gitlab), which the
    // reference resolves lexically to the project class and stays silent on;
    // witnessing it against the unrelated stdlib surface is an FP (caught by
    // fp_audit on gitlab app/models the first time this gate was
    // `knows_class`-wide).
    if index.class_name_of(interner, recv_ty).is_none() {
        // UNION receiver — the reference's `union_undefined_method_diagnostic`
        // (`check_rules.rb:1921`), reached exactly where the scalar path finds
        // no single concrete class (`class_name.nil?`). Fire only when EVERY
        // arm is a fully-known, bounded, non-nil instance class that lacks the
        // method — `x = [1, 2].tap { break "s"; break 1 }; x.push 3` witnesses
        // `"s" | 1` (rigor-rs#140). Any nil-bearing, safe-navigated,
        // Dynamic/unknown, singleton, metaclass, or module-mixin arm — or a
        // union whose arms collapse to ONE class — declines to silence, the
        // zero-FP direction the same reference encodes (`any?` permissive on
        // uncertainty). A single-class union (`"a" | "b"` — both String) is a
        // join artifact the scalar rule already owns.
        if let Type::Union(_) = interner.get(recv_ty) {
            return check_union_call(recv_ty, method, message_span, safe_nav, typer, interner, index);
        }
        if let Some(name) = typer.source().class_name_for_id_of(interner, recv_ty) {
            // `knows_toplevel_class` ALONE (ADR-0042 gate probe s5): a
            // TOPLEVEL project-sig class (`Widget`) is in the toplevel set via
            // its authoritative registration, so the former
            // `|| is_project_sig_class` arm only ever ADDED nested-only sig
            // classes (`module Outer; class Inner` reached by a bare `Inner`
            // read) — a short-key artifact door the reference does not have:
            // it witnesses `Outer::Inner` through the QUALIFIED path only and
            // keeps bare `Inner` (which resolves to nothing at runtime)
            // silent. Probed: rigor-rs fired `spni' for Inner` where the
            // reference is silent — an oracle FP shape, now closed.
            // ADR-0042 Slice 3: check the ISOLATED qualified surface, not the
            // short-key merge. A toplevel project-sig `Status` colliding with a
            // NESTED stdlib `Process::Status` no longer silently inherits
            // `exited?` from the stdlib class (fixture 70 — residual defect-2
            // unsoundness). For a non-colliding class or a toplevel-vs-toplevel
            // collision, this is identical to `class_has_method`.
            // ADR-0042 Slice 4: fire for a TOPLEVEL known class (unchanged) OR
            // a NESTED project-sig class (`Outer::Inner`) — the reference
            // witnesses the latter through the qualified path, which
            // `knows_toplevel_class` refuses for the defect-2 reason. Both use
            // the ISOLATED qualified surface.
            // MultiWrite substrate Slice 2: fire for a NESTED name the loaded
            // RBS models under its QUALIFIED key (`Process::Status`, reached
            // through `Process.wait2`'s tuple return). `knows_toplevel_class`
            // refuses every namespaced name for the defect-2 reason (a SHORT key
            // like `Status` may be a project class), but the qualified key is an
            // isolated entry that no project name can collide with — the same
            // argument the typer's `ConstantRead` arm already makes for
            // `ERB::Util`, and the same surface `qualified_class_has_method`
            // checks. Adds nothing for a bare name: a top-level class is already
            // in `knows_toplevel_class`, and a nested-only class's SHORT key has
            // no qualified entry.
            //
            // The DECLARATION-ONLY restriction was measured, not theorised —
            // but on a premise that has since EXPIRED. In 2026-07 rigor-rs's
            // surface for a namespaced GEM class was weaker than the oracle's
            // (the reference's `data/vendored_gem_sigs/`, then unvendored) and
            // `Gem::Version.new("1.0").segments` fired here while the oracle
            // stayed silent. `800b3a1` vendored those sigs on 2026-07-31; the
            // port's surface for those classes is now COMPLETE and that FP
            // cannot re-open (measured 2026-09-09, 651 names per class). What
            // holds the restriction up today is rigor-rs#123's 26 ancestor-
            // closure holes. See `SourceIndex::is_declaration_only_class` for
            // the full argument and the audit of what remains reachable.
            // A project class carries only its SHORT key here, so a bundled RBS
            // class of the same bare name is a DIFFERENT class and its method
            // table is the wrong surface to witness against — WHEREVER Ruby's
            // lexical lookup would reach the project one. The reference resolves
            // the constant lexically and stays silent there
            // (`RSpec::Core::DidYouMean#call` vs stdlib's `module DidYouMean`).
            // The gate is the SAME lexical predicate the C1 constant-shadow gate
            // uses, not a global "the project defines this name somewhere": a
            // gitlab-foss `Gitlab::Database::Partitioning::Time` must not silence
            // `Time.parse(x).in_time_zone` over in `Gitlab::GithubImport`, where
            // the project `Time` is not visible and the oracle does fire.
            // The shadow test applies ONLY to the bundled-RBS arm. A project
            // SIG class is authoritative for its own name (fixture 70), and the
            // declaration-only arm is already isolated by its qualified key.
            // Upstream #739/#742 — an INSTANCE receiver whose class is an RBS
            // module (its includer contributes an arbitrary surface) or the
            // generic `Class`/`Module`. Placed before the resolution gates for the
            // same reason the reference puts `unenumerable_receiver?` at the top of
            // the diagnostic: the question is about the receiver, not about which
            // surface happens to model it.
            if unenumerable_instance_receiver(index, name) {
                return None;
            }
            let use_prefix = typer.enclosing_prefix(message_span);
            let bundled_toplevel = index.knows_toplevel_class(name)
                && !typer.source().constant_shadowed(name, use_prefix);
            if (bundled_toplevel
                || index.is_qualified_project_sig_class(name)
                || (index.knows_qualified_class(name)
                    && typer.source().is_declaration_only_class(name)))
                && !index.qualified_class_has_method(name, method)
            {
                let receiver_render = render_receiver(interner, index, typer.source(), recv_ty);
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
        }
    }

    // Witness ONLY over a class the core (RBS/CORE_CLASSES) surface models and
    // round-trips by id. A receiver that resolves only through the in-source /
    // registry surface (a project class, or a non-core `X.new` like Pathname)
    // returns `None` here ⇒ silent (reference leniency, see the rustdoc above).
    let class_name = index.class_name_of(interner, recv_ty)?;
    // Upstream #739/#742 — the core-id twin of the decline above. `class_name_of`
    // answers only with a `CORE_CLASSES` name today, none of which is a module or a
    // metaclass, so this is defence in depth: it keeps the two instance paths
    // answering the same question the same way should that array ever widen.
    if unenumerable_instance_receiver(index, class_name) {
        return None;
    }
    if !index.knows_class(class_name) {
        return None;
    }
    if index.class_has_method(class_name, method) {
        return None;
    }
    // The project's OWN declaration of the method on this class wins over the RBS
    // surface, exactly as the reference's `source_declared_method?` gate does
    // (checked before it reaches `Reflection.rbs_class_known?`). A reopened core
    // class contributes methods RBS cannot know about — rake's `class String`
    // adds `#ext` and `#pathmap_explode` — and witnessing their absence against
    // RBS alone is a false positive.
    if typer.source().project_declares_method(typer.file_key(), class_name, method) {
        return None;
    }
    // `last_resort_surface_answers?`'s `ancestry_declares_method?` — the same
    // question asked through the project's OWN ancestry (`class String;
    // include M` makes `String#m` exist for Rigor without touching RBS).
    // Asked last, exactly like the reference: this is the one probe that
    // walks the class graph.
    if typer
        .source()
        .project_declares_method_through_ancestors(typer.file_key(), class_name, method)
    {
        return None;
    }

    // We have witnessed absence over a core/RBS class. Render the receiver in the
    // reference's spelling (value-pinned for a Constant/Tuple, else the class
    // name) via the shared display layer.
    let receiver_render = render_receiver(interner, index, typer.source(), recv_ty);
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

/// `call.undefined-method` over a UNION receiver — the port of the reference's
/// `union_undefined_method_diagnostic` (`check_rules.rb:1921`), reached where
/// the scalar path finds no single concrete class. It fires only when EVERY
/// arm is a fully-known, bounded, non-nil, instance-side class that lacks the
/// method (`"s" | 1` calling `push`), mirroring the reference's gate order:
///
/// - `safe_navigation?` declines outright;
/// - any nil arm (`Constant[nil]` / `NilClass`) keeps `T | nil` silent — the
///   deliberate N3 decision (ADR-62);
/// - any UNANSWERABLE arm — neither `class_name_of` nor the source-registry
///   `class_name_for_id_of` resolving a name (Dynamic / Top / Bot /
///   Singleton — the reference's explicit singleton bail), a metaclass
///   (`Class` / `Module`), an RBS module mixin, or a registry name that fails
///   the scalar path's witness gates — declines, since no sound "absent on
///   every arm" verdict exists there;
/// - a class the bundled surface does not model (`!knows_class`) is likewise
///   unanswerable — the reference's permissive `return true` from
///   `method_present_anywhere?`;
/// - the method PRESENT on any arm (project `def` — `source_declared_method?`
///   — or the conservative `class_has_method` surface) declines;
/// - fewer than two DISTINCT arm classes (`"a" | "b"`, one `String`) is a
///   join artifact — the scalar rule's job, never this one's.
fn check_union_call(
    recv_ty: rigor_types::TypeId,
    method: &str,
    message_span: (usize, usize),
    safe_nav: bool,
    typer: &Typer,
    interner: &mut Interner,
    index: &CoreIndex,
) -> Option<Diagnostic> {
    let Type::Union(members) = interner.get(recv_ty) else {
        return None;
    };
    let members = members.clone();
    if safe_nav {
        return None;
    }
    let mut arm_classes: Vec<String> = Vec::new();
    for member in members {
        let is_nil_member = matches!(interner.get(member), Type::Constant(Scalar::Nil))
            || index.class_name_of(interner, member) == Some("NilClass");
        if is_nil_member {
            return None;
        }
        // A member's class resolves two ways: a core-id name (`class_name_of`)
        // or a source-registry `Nominal` (`class_name_for_id_of`) — the latter
        // is what `rescue A, B => e` mints for a named RBS class
        // (`rescue ArgumentError, TypeError => e` binds `e` to
        // `ArgumentError | TypeError` on the oracle, and `e.w` fires the
        // union diagnostic).
        match index
            .class_name_of(interner, member)
            .map(|n| (n.to_string(), false))
            .or_else(|| {
                typer
                    .source()
                    .class_name_for_id_of(interner, member)
                    .map(|n| (n.to_string(), true))
            }) {
            None => return None,
            Some((class_name, registry_arm)) => {
                if unenumerable_instance_receiver(index, &class_name) {
                    return None;
                }
                if registry_arm {
                    // The scalar path's source-registry gate: witness only a
                    // bundled toplevel class the use site does not shadow, a
                    // project-sig class, or a declaration-only qualified one.
                    let use_prefix = typer.enclosing_prefix(message_span);
                    let witnessable = (index.knows_toplevel_class(&class_name)
                        && !typer
                            .source()
                            .constant_shadowed(&class_name, use_prefix))
                        || index.is_qualified_project_sig_class(&class_name)
                        || (index.knows_qualified_class(&class_name)
                            && typer.source().is_declaration_only_class(&class_name));
                    if !witnessable
                        || index.qualified_class_has_method(&class_name, method)
                        || typer.source().project_declares_method(
                            typer.file_key(),
                            &class_name,
                            method,
                        )
                        || typer.source().project_declares_method_through_ancestors(
                            typer.file_key(),
                            &class_name,
                            method,
                        )
                    {
                        return None;
                    }
                } else {
                    if !index.knows_class(&class_name) {
                        return None;
                    }
                    if typer.source().project_declares_method(
                        typer.file_key(),
                        &class_name,
                        method,
                    ) || index.class_has_method(&class_name, method)
                        || typer.source().project_declares_method_through_ancestors(
                            typer.file_key(),
                            &class_name,
                            method,
                        )
                    {
                        return None;
                    }
                }
                arm_classes.push(class_name);
            }
        }
    }
    if arm_classes
        .iter()
        .collect::<std::collections::HashSet<_>>()
        .len()
        < 2
    {
        return None;
    }
    let receiver_render = render_receiver(interner, index, typer.source(), recv_ty);
    let message = format!("undefined method `{method}' for {receiver_render}");
    let severity = catalog(CALL_UNDEFINED_METHOD)
        .map(|e| e.default_severity)
        .unwrap_or(Severity::Error);
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

/// Apply `call.undefined-method` to a call whose bare-local receiver was
/// class-narrowed by the flow pass (census mechanism 1) — the `is_a?`/
/// `case-when` counterpart of [`check_call`], firing from the precomputed
/// [`rigor_infer::Typer::class_narrowing_snapshots`] map.
///
/// The FP-delicate flow reasoning (which guard shapes narrow, truthy-edge-only,
/// invalidation, block-scope discipline) lives in the snapshot pass; here we
/// apply the residual gates, each `None` FP-safe:
/// 1. NOT a safe-nav call (belt-and-braces — the pass also skips them).
/// 2. The call node is in the snapshot map with a class name `C`.
/// 3. The receiver still types `Dynamic`/`Top` at the use site (the narrowing
///    only ever REPLACES a Dynamic/Top carrier — `narrow_class_other`
///    semantics; any concrete carrier means the ordinary [`check_call`] path
///    owns the site and has already declined).
/// 4. The witnessing tail mirrors [`check_call`]'s QUALIFIED path over
///    `Nominal[C]`: `C` resolves on one of the three accepted surfaces (a
///    genuine top-level RBS class, a project-`sig/` class, or the bundled
///    qualified registry), the method is certainly ABSENT on its ISOLATED
///    qualified surface, and the project does not reopen `C` with the method
///    (`project_declares_method`). `C` arrives already resolved to a qualified
///    key — see `Typer::resolve_constant_as_written`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn check_narrowed_call(
    call_id: rigor_parse::NodeId,
    ast: &LoweredAst,
    receiver: rigor_parse::NodeId,
    method: &str,
    message_span: (usize, usize),
    safe_nav: bool,
    env: &rigor_infer::TypeEnv,
    typer: &Typer,
    interner: &mut Interner,
    index: &CoreIndex,
    snapshots: &std::collections::HashMap<rigor_parse::NodeId, String>,
) -> Option<Diagnostic> {
    // (1) Safe-nav dispatch is out of the narrowed envelope.
    if safe_nav {
        return None;
    }
    // (2) The flow pass must have narrowed this exact call's receiver.
    let class_name = snapshots.get(&call_id)?.as_str();
    // (3) Dynamic/Top-only: any concrete carrier at the use site declines.
    let rty = typer.type_of(ast, receiver, env, interner);
    if !matches!(interner.get(rty), Type::Dynamic(_) | Type::Top) {
        return None;
    }
    // (4) Witness absence over the RESOLVED surface, exactly as `check_call`'s
    // qualified tail. S2 (2026-08-08) replaced the old pair of gates —
    // `knows_toplevel_class` AND `CoreIndex::class_id` — with ONE resolution
    // path. Each was an independent blocker:
    //   * `knows_toplevel_class` refuses EVERY namespaced name (the defect-2
    //     rule, which stays in force for every other consumer — this slice adds
    //     a path, it does not invert the gate);
    //   * `class_id` interns over `CORE_CLASSES`, a NINE-element array, so the
    //     witness fired for those nine names only. `Time`/`Range`/`Struct`/
    //     `Pathname` guards are reference-firing and were rigor-rs-silent
    //     (probes u1/u2/u4/u5) despite passing `knows_toplevel_class`.
    // The name arrives already resolved to a qualified key
    // (`Typer::resolve_constant_as_written`, at mint time where the use site's
    // lexical prefix is available), so all three spellings of a class — bare,
    // qualified, `::`-absolute — reach the same surface and the same rendering.
    //
    // Upstream #739/#742 (pin `v0.3.8`) — the guard NAMES the receiver's class,
    // and when that class is an RBS module or the generic `Class` / `Module` the
    // surface is not enumerable. This is the path the whole F-C family actually
    // fires from: `return unless v.is_a?(Digest::Instance)` types `v` through the
    // narrowing snapshot, not through a carrier `check_call` can see. `class_name`
    // arrives already resolved to a QUALIFIED key
    // (`Typer::resolve_constant_as_written`), which is exactly the spelling
    // `is_qualified_module` wants.
    //
    // Note what this does NOT touch: an `Enumerable` guard on a value already
    // typed `Array` keeps the `Array` bound (subclass ordering), so `class_name`
    // there is `"Array"` and the witness still fires on both engines — row x11,
    // the control an "any module in the guard" test would have silenced.
    if unenumerable_instance_receiver(index, class_name) {
        return None;
    }
    // Accepted surfaces: the existing top-level one, the project's own `sig/`
    // (nested included — probes q2/q3/q8), and the bundled qualified registry.
    // Everything else DECLINES, which is free: an unresolvable name (p2/p2b)
    // and an in-source-only project class (ADR-0033 provenance, p5/q1) are both
    // reference-silent.
    if !index.knows_toplevel_class(class_name)
        && !index.is_qualified_project_sig_class(class_name)
        && !index.knows_qualified_class(class_name)
    {
        return None;
    }
    // The ISOLATED qualified surface, as ADR-0042 Slice 3 established for
    // `check_call`: a project class named `Status` must not inherit stdlib
    // `Process::Status`'s methods. For a top-level name the qualified key IS the
    // short key, so this is the same surface the old `class_has_method` read,
    // minus the short-key merge.
    if index.qualified_class_has_method(class_name, method) {
        return None;
    }
    // A project reopen MERGES with the RBS surface rather than replacing it
    // (probe q6): the reopened method silences, the still-absent one still
    // fires. Keyed as written, so `Proj::Thing` matches.
    if typer.source().project_declares_method(typer.file_key(), class_name, method) {
        return None;
    }
    // Render the narrowed receiver as the reference does — the FULL resolved
    // path ("undefined method `frobnicate_zzz' for URI::HTTP", probes §1/§3),
    // which for a core name is the same plain class name `Nominal[C]` rendered
    // before ("… for Hash"). No `ClassId` is needed: `render_receiver` on a
    // `Nominal` yields exactly its class name.
    let receiver_render = class_name.to_string();
    let message = format!("undefined method `{method}' for {receiver_render}");
    let severity = catalog(CALL_UNDEFINED_METHOD)
        .map(|e| e.default_severity)
        .unwrap_or(Severity::Error);
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

/// Apply `call.undefined-method` to a call whose bare-local receiver carries a
/// COLLECTION shape the flow pass threaded — the mutation/local-binding
/// counterpart of [`check_call`], firing from the precomputed
/// [`rigor_infer::Typer::collection_shape_snapshots`] map (spec
/// docs/notes/20260807-collection-shape-slice-spec.md, stage 1).
///
/// All the FP-delicate flow reasoning (which seeds mint a carrier, which
/// mutators keep the nominal, how branch joins and block bodies decline) lives in
/// the snapshot pass. Here we apply the residual gates, each `None` FP-safe, in
/// exact parallel with [`check_narrowed_call`]:
/// 1. NOT a safe-nav call (belt-and-braces — the pass also skips them).
/// 2. The call node is in the snapshot map with a class name `C` ("Array"/"Hash").
/// 3. The receiver still types `Dynamic`/`Top` at the use site. This is what
///    keeps the rule to its intended job: at TOP level [`ScopedEnv`] already
///    binds the local, so [`check_call`] owns the site and has already decided
///    it; only a use inside a `def` body (empty scoped env ⇒ Dynamic) reaches
///    here, which is exactly the coverage gap the slice closes.
/// 4. The witnessing tail mirrors [`check_call`]'s core path over `Nominal[C]`.
///
/// [`ScopedEnv`]: crate::ScopedEnv
#[allow(clippy::too_many_arguments)]
pub(crate) fn check_collection_call(
    call_id: rigor_parse::NodeId,
    ast: &LoweredAst,
    receiver: rigor_parse::NodeId,
    method: &str,
    message_span: (usize, usize),
    safe_nav: bool,
    env: &rigor_infer::TypeEnv,
    typer: &Typer,
    interner: &mut Interner,
    index: &CoreIndex,
    snapshots: &std::collections::HashMap<rigor_parse::NodeId, &'static str>,
) -> Option<Diagnostic> {
    // (1) Safe-nav dispatch is out of the envelope.
    if safe_nav {
        return None;
    }
    // (2) The flow pass must have typed this exact call's receiver.
    let class_name = *snapshots.get(&call_id)?;
    // (3) Dynamic/Top-only: any concrete carrier at the use site declines.
    let rty = typer.type_of(ast, receiver, env, interner);
    if !matches!(interner.get(rty), Type::Dynamic(_) | Type::Top) {
        return None;
    }
    // (4) Witness absence over the core surface, exactly as `check_call`'s tail.
    if !index.knows_toplevel_class(class_name) {
        return None;
    }
    if index.class_has_method(class_name, method) {
        return None;
    }
    if typer.source().project_declares_method(typer.file_key(), class_name, method) {
        return None;
    }
    let class = index.class_id(class_name)?;
    let recv_ty = interner.intern(Type::Nominal { class, args: vec![] });
    let receiver_render = render_receiver(interner, index, typer.source(), recv_ty);
    let message = format!("undefined method `{method}' for {receiver_render}");
    let severity = catalog(CALL_UNDEFINED_METHOD)
        .map(|e| e.default_severity)
        .unwrap_or(Severity::Error);
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

/// Apply `call.possible-nil-receiver` to a single call, firing from the
/// precomputed ADR-0038 Slice-1 snapshot map.
///
/// The FP-delicate flow reasoning (which receiver is certainly `C | nil` and
/// unguarded) lives in [`rigor_infer::Typer::nilable_receiver_snapshots`], which
/// threads the nilability fact straight-line through the program INCLUDING block
/// bodies (the treemaps cluster). Here we only apply the two RBS gates the arm
/// still needs, in order (every `None` is FP-safe):
/// 1. NOT a safe-nav call (`x&.foo` short-circuits on nil ⇒ not a bug). The
///    snapshot pass also skips safe-nav uses; this is a belt-and-braces re-check.
/// 2. The call node is in `snapshots` with a non-nil core arm `C` (the pass
///    proved a certain `C | nil`, unguarded receiver).
/// 3. `method` is ABSENT on `NilClass` (else the call is sound on the nil arm —
///    `to_s`/`to_a`/`inspect`/`nil?`/… live on NilClass and must not fire).
/// 4. `method` is PRESENT on `C` (the non-nil arm defines it — otherwise this is
///    `call.undefined-method`'s job, one diagnostic per call site).
pub(crate) fn check_nil_receiver(
    call_id: rigor_parse::NodeId,
    method: &str,
    message_span: (usize, usize),
    safe_nav: bool,
    snapshots: &std::collections::HashMap<rigor_parse::NodeId, &'static str>,
    index: &CoreIndex,
) -> Option<Diagnostic> {
    // (1) Safe-nav calls short-circuit on nil at runtime ⇒ never a bug.
    if safe_nav {
        return None;
    }
    // (2) The flow pass must have proved a certain `C | nil`, unguarded receiver.
    let core_arm = *snapshots.get(&call_id)?;
    // (3) The method must be ABSENT on NilClass (else sound on the nil arm).
    if index.class_has_method("NilClass", method) {
        return None;
    }
    // (4) The method must be PRESENT on the non-nil arm `C` (else this is
    // `call.undefined-method`'s call, not ours — one diagnostic per site).
    if !index.class_has_method(core_arm, method) {
        return None;
    }
    // Fire. Message is byte-exact with the reference's
    // `build_nil_receiver_diagnostic`: ``possible nil receiver: `m' is undefined
    // on NilClass``. Severity resolves to the catalog default (`error` under
    // balanced — matching the reference's severity_profile).
    let message = format!("possible nil receiver: `{method}' is undefined on NilClass");
    let severity = catalog(CALL_POSSIBLE_NIL_RECEIVER)
        .map(|e| e.default_severity)
        .unwrap_or(Severity::Error);
    Some(Diagnostic {
        rule_id: CALL_POSSIBLE_NIL_RECEIVER,
        start_offset: message_span.0,
        end_offset: message_span.1,
        message,
        severity,
        source_family: "builtin",
        receiver_type: None,
        method_name: Some(method.to_string()),
    })
}
