use super::*;

/// The build-time-embedded set is present and carries the core classes.
#[test]
fn embedded_rbs_non_empty_with_core_files() {
    assert!(!EMBEDDED_RBS.is_empty(), "EMBEDDED_RBS must not be empty");
    let has = |needle: &str| EMBEDDED_RBS.iter().any(|(p, _)| p.ends_with(needle));
    assert!(has("core/array.rbs"), "missing core/array.rbs");
    assert!(has("core/string.rbs"), "missing core/string.rbs");
    // A stdlib closure member.
    assert!(
        EMBEDDED_RBS.iter().any(|(p, _)| p.contains("stdlib/pathname/")),
        "missing stdlib/pathname"
    );
    // Entries are non-empty file contents.
    assert!(EMBEDDED_RBS.iter().all(|(_, c)| !c.is_empty()));
}

/// `CoreData::load()` with `RIGOR_RBS_CORE_DIR` UNSET ingests the embedded
/// set (NOT a stub): it knows the core roots and stdlib-closure classes, and
/// resolves instance methods with the same parity the runtime path gives.
///
/// NB: this asserts on the global `load()` and so must not clobber the env
/// var (other code/tests share the process). It relies on the harness/CI
/// running with the var unset; if it happens to be set, the override path is
/// equally valid for these assertions (same signatures), so we don't gate.
#[test]
fn embedded_load_is_non_stub_and_method_parity() {
    let idx = CoreData::load();
    // Core classes from the embedded core/ tree.
    assert!(idx.knows_class("String"));
    assert!(idx.knows_class("Array"));
    assert!(idx.knows_class("Hash"));
    // Stdlib-closure classes (proves the stdlib set embedded, not just core
    // / the small stub): `Pathname` (pathname) and `Set` (in core builtin).
    assert!(idx.knows_class("Pathname"));
    assert!(idx.knows_class("Set"));
    // Method existence parity for a known method, and absence for a typo.
    assert!(idx.class_has_method("String", "upcase"));
    assert!(!idx.class_has_method("String", "lenght"));
}

/// The rigor-owned `overlay/` tree is embedded and ingested — the
/// reference's `data/vendored_gem_sigs/` and `data/core_overlay/`, which it
/// loads in EVERY run. Without them rigor-rs's surface is strictly weaker
/// than the oracle's and reports methods the oracle resolves — measured on
/// rigor-survey as `::DidYouMean.formatter`.
#[test]
fn embedded_overlay_is_loaded() {
    assert!(
        EMBEDDED_RBS.iter().any(|(p, _)| p.starts_with("overlay/")),
        "overlay tree not embedded"
    );
    let idx = CoreData::load();
    // `data/vendored_gem_sigs/did_you_mean/did_you_mean_extras.rbs` — the
    // upstream rbs `stdlib/did_you_mean` declares neither.
    assert!(idx.class_has_singleton_method("DidYouMean", "formatter"));
    assert!(idx.class_has_singleton_method("DidYouMean", "correct_error"));
    // A genuine typo on the same module still witnesses absent.
    assert!(!idx.class_has_singleton_method("DidYouMean", "totally_bogus_name"));
    // `prism` is deliberately NOT copied: its file supplements the prism
    // gem's own sig/, which this tree does not vendor, so loading it alone
    // would declare `module Prism` WITHOUT `Prism.parse` and witness a false
    // absence there. See vendor/rbs/PROVENANCE.md.
    assert!(
        !EMBEDDED_RBS.iter().any(|(p, _)| p.contains("overlay/vendored_gem_sigs/prism")),
        "prism supplement must stay out of the overlay"
    );
}

/// The `e59b7b89` re-sync's three new `data/core_overlay/` files parse (a
/// parse failure drops the WHOLE file silently in `Builder::ingest`), and
/// `hash_rbs3.rbs` stays out: the reference loads it only on the rbs `< 4.0`
/// line (`RbsLoader::RBS_LINE_CORE_OVERLAYS`), and this tree is rbs 4.2,
/// which declares its `transform_keys` overloads upstream — loading the
/// `| ...` continuation here would duplicate them.
#[test]
fn e59b_core_overlays_parse_and_hash_rbs3_is_excluded() {
    for name in ["string_io.rbs", "enumerable.rbs", "enumerator.rbs"] {
        let (_, contents) = EMBEDDED_RBS
            .iter()
            .find(|(p, _)| *p == format!("overlay/core_overlay/{name}"))
            .unwrap_or_else(|| panic!("overlay/core_overlay/{name} not embedded"));
        assert!(parse(contents).is_ok(), "{name} must parse");
    }
    assert!(
        !EMBEDDED_RBS.iter().any(|(p, _)| p.ends_with("hash_rbs3.rbs")),
        "hash_rbs3.rbs is gated to rbs < 4.0 upstream and must stay out"
    );
    // `string_io.rbs`: StringIO includes Enumerable[String].
    let idx = CoreData::load();
    assert!(idx.class_has_method("StringIO", "detect"));
    assert!(!idx.class_has_method("StringIO", "totally_bogus_name"));
}

/// Step 1 (nilable-RBS-return): an `Optional` return (`String?`) is
/// preserved as `(class, nilable=true)`; a plain return is `(class, false)`;
/// and overloads that DISAGREE on nilability collapse to `None` (never
/// invent nil). `byteslice` is uniformly `-> String?`; `upcase` is plainly
/// `-> String`; `try_convert`'s overloads mix `String` and `String?`.
#[test]
fn nilable_return_preserved_and_conservative() {
    let idx = CoreData::load();
    // Nilable return: `String#byteslice : (...) -> String?` ⇒ (String, true).
    assert_eq!(
        idx.method_return_nilable("String", "byteslice"),
        Some(("String", true)),
        "byteslice's String? must surface nilable=true"
    );
    // Plain return: `String#upcase : () -> String` ⇒ (String, false).
    assert_eq!(
        idx.method_return_nilable("String", "upcase"),
        Some(("String", false)),
        "upcase's plain String must surface nilable=false"
    );
    // Disagreeing overloads (String vs String?) ⇒ conservative None.
    assert_eq!(
        idx.method_return_nilable("String", "try_convert"),
        None,
        "overloads disagreeing on nilability must collapse to None"
    );
    // The existing non-nil accessors are unchanged by the new bit.
    assert_eq!(idx.method_return("String", "upcase"), Some("String"));
    assert_eq!(idx.method_return("String", "byteslice"), Some("String"));
}

/// rbs 4.1 rewrote several core returns from a spelled-out generic to the
/// late-bound `instance` (`Hash#compact: () -> ::Hash[K, V]` became `() ->
/// instance`). On an INSTANCE method that means "an instance of the
/// receiver's class", so the return must still resolve to a concrete class —
/// otherwise a chained call (`h.compact.presence`) types Dynamic and every
/// rule that rides the receiver type goes silent.
#[test]
fn instance_return_resolves_to_the_receiver_class() {
    let idx = CoreData::load();
    assert_eq!(
        idx.method_return("Hash", "compact"),
        Some("Hash"),
        "`-> instance` on Hash#compact must resolve to the receiver class"
    );
    // The sentinel must never escape as a class name, on any accessor.
    assert_eq!(idx.method_return_nilable("Hash", "compact"), Some(("Hash", false)));
    assert_eq!(idx.declared_instance_return("Hash", "compact"), Some(Some("Hash")));
    // A receiver the index does not model declines to None (⇒ Dynamic).
    assert_eq!(idx.method_return("NoSuchClassZzz", "compact"), None);
}

/// `-> self` on an INSTANCE method is the receiver itself — the spelling
/// core uses for the mutating and re-tagging families. It was tracked only
/// on the singleton path, so an instance method's `-> self` collapsed to
/// `Dynamic` and broke every chain running through it (measured: gitlab-foss
/// `[error.message].concat(error.backtrace).join("\n").truncate(N)`).
#[test]
fn self_return_on_an_instance_method_resolves_to_the_receiver() {
    let idx = CoreData::load();
    assert_eq!(idx.method_return("Array", "concat"), Some("Array"));
    assert_eq!(idx.method_return("Array", "push"), Some("Array"));
    assert_eq!(idx.method_return("String", "force_encoding"), Some("String"));
    // Resolved against the RECEIVER, so a subclass keeps its own type
    // rather than widening to the class that declared the method.
    assert_eq!(idx.method_return("Symbol", "freeze"), Some("Symbol"));
    // A singleton `-> self` is the class OBJECT, which this flat slot
    // cannot spell — it must keep declining instead of being folded like
    // the instance case.
    assert_eq!(idx.singleton_method_return("Struct", "new"), None);
}

/// rbs 4.1 also started using BOUNDED method type parameters in core
/// signatures (`Array#fetch`'s block overload spells its index `[I < _ToInt,
/// T] (I index)`). A bare variable is an opaque `Other` leaf that admits
/// every argument, which would silence the mismatch the other overloads
/// report — so a bounded variable must resolve to its declared bound.
#[test]
fn bounded_method_type_param_resolves_to_its_upper_bound() {
    let idx = CoreData::load();
    let overloads = idx.method_overloads("Array", "fetch").expect("Array#fetch");
    let params: Vec<&RetainedParamType> = overloads
        .iter()
        .filter_map(|o| o.required_positionals.first())
        .collect();
    assert!(
        params.iter().any(|p| matches!(p, RetainedParamType::Interface("_ToInt"))),
        "the bounded `I < _ToInt` index param must carry its bound: {params:?}"
    );
    assert!(
        !params.iter().any(|p| matches!(p, RetainedParamType::Other(s) if s == "I")),
        "no overload may keep the bare variable as an admit-everything leaf: {params:?}"
    );
}

/// ADR-0033: an empty `sig_dirs` is byte-identical to `load_with_plugins`
/// (the gating contract) — the no-`sig/` path must be unchanged.
#[test]
fn empty_project_sig_is_unchanged() {
    let base = CoreData::load_with_plugins(&[]);
    let with_sig = CoreData::load_for_project(&[], &[]);
    assert_eq!(base.class_count(), with_sig.class_count());
    // A named-but-absent dir is inert too (ingestion skips a non-directory).
    let absent = CoreData::load_for_project(
        &[],
        &[std::path::PathBuf::from("this-dir-does-not-exist-xyzzy")],
    );
    assert_eq!(base.class_count(), absent.class_count());
}

/// ADR-14 slice 10: the sig-gen-only precise declared-return accessors are
/// three-valued and never "assume present". `Object#hash → Integer` resolves
/// through the ancestor chain (instance AND — via the class object's own
/// ancestry — singleton), an absent method on a complete chain is `None`
/// (NotDeclared), and an unresolvable-return method is `Some(None)`.
#[test]
fn sig_gen_declared_return_accessors_are_three_valued() {
    let idx = CoreData::load();
    // Instance: declared, concrete return.
    assert_eq!(idx.declared_instance_return("String", "upcase"), Some(Some("String")));
    assert_eq!(idx.declared_instance_return("Object", "hash"), Some(Some("Integer")));
    // Instance: declared, but the return is not a single bare concrete class
    // (`Integer#times` returns an Enumerator/self union) ⇒ Some(None).
    assert_eq!(idx.declared_instance_return("Integer", "times"), Some(None));
    // Instance: not declared on a fully-loaded chain ⇒ None (NotDeclared).
    assert_eq!(idx.declared_instance_return("String", "definitely_absent_zzz"), None);
    // An unknown class ⇒ None (the SigEnv gates on presence first).
    assert_eq!(idx.declared_instance_return("NoSuchClassZzz", "foo"), None);

    // Singleton: the class object inherits `Object#hash` (Integer) through
    // its `Class`/`Module`/`Object` ancestry.
    assert_eq!(idx.declared_singleton_return("String", "hash"), Some(Some("Integer")));
    // Singleton: an absent class method on a complete surface ⇒ None.
    assert_eq!(idx.declared_singleton_return("String", "definitely_absent_zzz"), None);

    // Chain completeness: a fully-loaded core class is complete.
    assert!(idx.chain_complete("String"));
    assert!(!idx.chain_complete("NoSuchClassZzz"));
}

/// ADR-0033: a project `sig/` dir's classes join the known set (so the
/// dispatch rules can witness them) and their methods resolve, while a typo
/// on such a class is witnessed-absent — exactly the reference's
/// `rbs_class_known?` behaviour. Uses a real temp dir since ingestion is
/// filesystem-driven (`ingest_rbs_dir`).
#[test]
fn project_sig_widens_known_classes() {
    let base = std::env::temp_dir()
        .join(format!("rigor-sig-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).expect("temp sig dir");
    std::fs::write(
        base.join("widget.rbs"),
        "class Widget\n  def spin: () -> Integer\nend\n",
    )
    .expect("write sig");

    let data = CoreData::load_for_project(&[], std::slice::from_ref(&base));
    assert!(data.knows_class("Widget"), "project class joins knows_class");
    assert!(data.knows_toplevel_class("Widget"), "declared at top level");
    assert!(data.class_has_method("Widget", "spin"), "declared method resolves");
    // A typo is witnessed-absent (the whole ancestor chain — Widget + Object
    // — is loaded, so absence is decidable), the coverage this leg unlocks.
    assert!(!data.class_has_method("Widget", "spni"));
    // Core classes are unaffected by the project ingest.
    assert!(data.knows_class("String"));
    assert!(data.class_has_method("String", "upcase"));

    let _ = std::fs::remove_dir_all(&base);
}

// -- ATM shared-substrate (Slice 1) retention tests ------------------------

/// Per-overload retention on a real multi-overload core method: the vendored
/// core `Integer#+` has FOUR overloads (`Integer`/`Float`/`Rational`/
/// `Complex`), and the stdlib `bigdecimal` reopen (`def +: (BigDecimal) ->
/// BigDecimal | ...` — an OVERLOADING def whose trailing `...` appends the
/// existing overloads) PREPENDS a fifth. Each has a single required
/// positional whose type is the concrete numeric class; the reference's
/// multi-overload label renders them in exactly this order (`5 + nil` ⇒
/// `expected BigDecimal | Integer | Float | Rational | Complex`). The merged
/// arity path (unchanged) still sees one `(1, Some(1))` envelope.
#[test]
fn atm_per_overload_retention_integer_plus() {
    let idx = CoreData::load();
    let ov = idx
        .method_overloads("Integer", "+")
        .expect("Integer#+ has retained overloads");
    assert_eq!(ov.len(), 5, "Integer#+ has five overloads (bigdecimal reopen + core four)");
    let names: Vec<&RetainedParamType> = ov
        .iter()
        .map(|o| {
            assert_eq!(o.required_positionals.len(), 1, "one required positional");
            assert!(o.optional_positionals.is_empty());
            assert!(!o.has_rest_positionals);
            assert!(!o.has_required_keywords);
            assert!(!o.has_optional_keywords);
            assert!(!o.has_rest_keywords);
            assert!(!o.has_trailing_positionals);
            &o.required_positionals[0]
        })
        .collect();
    assert_eq!(
        names,
        vec![
            &RetainedParamType::ClassInstance("BigDecimal"),
            &RetainedParamType::ClassInstance("Integer"),
            &RetainedParamType::ClassInstance("Float"),
            &RetainedParamType::ClassInstance("Rational"),
            &RetainedParamType::ClassInstance("Complex"),
        ],
        "the overloading reopen's operand comes first, then the core four"
    );
    // The merged arity envelope is untouched (additive-retention contract).
    assert_eq!(idx.method_arity("Integer", "+"), Some((1, Some(1))));
    // Param-NAME retention (message prefix substrate): `String#+` declares
    // `(string other_string)`, so the name rides alongside the type.
    let plus = idx.method_overloads("String", "+").expect("String#+ retained");
    assert_eq!(plus.len(), 1);
    assert_eq!(plus[0].required_positional_names, vec![Some("other_string")]);
}

/// Type-alias retention against the ACTUAL vendored RBS: `builtin.rbs`
/// declares `type string = String | _ToStr`, so `resolve_type_alias("string")`
/// is a two-arm union of the concrete `String` class and the `_ToStr`
/// interface — retained RAW (the interface is a leaf, not expanded).
#[test]
fn atm_type_alias_retention_string() {
    let idx = CoreData::load();
    let rhs = idx
        .resolve_type_alias("string")
        .expect("`type string` is retained");
    assert_eq!(
        rhs,
        &RetainedParamType::Union(vec![
            RetainedParamType::ClassInstance("String"),
            RetainedParamType::Interface("_ToStr"),
        ]),
        "type string = String | _ToStr"
    );
    // A leading `::` on the query is tolerated.
    assert!(idx.resolve_type_alias("::string").is_some());
}

/// Interface method-name retention against the ACTUAL vendored RBS:
/// `interface _ToStr; def to_str: () -> String; end` ⇒ `["to_str"]`.
#[test]
fn atm_interface_method_names_to_str() {
    let idx = CoreData::load();
    assert_eq!(
        idx.interface_methods("_ToStr"),
        Some(["to_str"].as_slice()),
        "_ToStr requires exactly to_str"
    );
    // A richer interface (`_Each` requires `each`) is retained too.
    let each = idx.interface_methods("_Each").expect("_Each retained");
    assert!(each.contains(&"each"), "_Each requires each");
}

/// Keyword / rest / optional / trailing presence flags are set from the real
/// RBS. `String#gsub` has an overload with optional positionals and one with
/// a block; `Hash#merge` takes a rest positional. We assert the flags rather
/// than pin exact overload indices (which vary with the vendored RBS).
#[test]
fn atm_presence_flags_from_real_rbs() {
    let idx = CoreData::load();
    // `String#*` : (int) -> String — a single required positional, no rest.
    let star = idx.method_overloads("String", "*").expect("String#* overloads");
    assert!(star.iter().all(|o| !o.has_rest_positionals));
    // `Array#push` / `Array#concat` take rest positionals (`*T`).
    let push = idx.method_overloads("Array", "push").expect("Array#push overloads");
    assert!(
        push.iter().any(|o| o.has_rest_positionals),
        "Array#push declares a rest positional"
    );
    // Optional positional retention: `String#chomp : (?string) -> String`.
    let chomp = idx.method_overloads("String", "chomp").expect("String#chomp overloads");
    assert!(
        chomp.iter().any(|o| !o.optional_positionals.is_empty()),
        "String#chomp has an optional positional overload"
    );
}

/// Overloads resolve over the ancestor chain AND through instance aliases,
/// exactly like the merged lookups. `Integer` inherits nothing for `+` (own
/// method); test an inherited case: `Integer#succ` is own, but `String#size`
/// is `alias size length` ⇒ overloads resolve via the alias target.
#[test]
fn atm_overloads_resolve_via_alias_and_chain() {
    let idx = CoreData::load();
    // `String#size` is `alias size length`; overloads resolve to `length`'s.
    let size = idx.method_overloads("String", "size");
    let length = idx.method_overloads("String", "length");
    assert!(size.is_some(), "aliased size resolves to length's overloads");
    assert_eq!(size, length, "size and length share the overload set");
    // Unknown method ⇒ None.
    assert!(idx.method_overloads("String", "definitely_absent_zzz").is_none());
    // Unknown class ⇒ None.
    assert!(idx.method_overloads("NoSuchClassZzz", "foo").is_none());
}

/// Class-method (singleton) overload retention — the ATM substrate for
/// `CGI.parse(...)` / `Base64.decode64(...)`. `CGI.parse` is a plain
/// `def self.parse: (String query) -> ...`, so it lives ONLY in the singleton
/// overload table (the instance `method_overloads` does not carry it).
#[test]
fn atm_singleton_method_overloads_retained() {
    let idx = CoreData::load();
    // Only assert when the stdlib RBS is actually loaded (a stub build has no
    // CGI); this keeps the test meaningful without failing a Ruby-free CI.
    if idx.knows_class("CGI") {
        let parse = idx
            .singleton_method_overloads("CGI", "parse")
            .expect("CGI.parse singleton overloads retained");
        assert!(
            parse
                .iter()
                .any(|o| o.required_positionals.len() == 1
                    && matches!(o.required_positionals[0], RetainedParamType::ClassInstance("String"))),
            "CGI.parse takes a single String positional: {parse:?}"
        );
        // A plain `def self.parse` is NOT an instance method.
        assert!(
            idx.method_overloads("CGI", "parse").is_none(),
            "CGI#parse is not an instance method"
        );
    }
    // Unknown singleton method / class ⇒ None.
    assert!(idx.singleton_method_overloads("Array", "definitely_absent_zzz").is_none());
    assert!(idx.singleton_method_overloads("NoSuchClassZzz", "foo").is_none());
}

/// Alias / interface cycle guard: a self-referential `type` alias
/// (`type loop_t = loop_t`) and a mutual pair are ingested WITHOUT any
/// build-time loop (the RHS is retained one level deep), and their raw tags
/// come back as `Alias(..)` leaves. Uses a project `sig/` dir since ingestion
/// is filesystem/parser-driven.
#[test]
fn atm_alias_cycle_guard_and_nested_retention() {
    let base = std::env::temp_dir()
        .join(format!("rigor-atm-cycle-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).expect("temp sig dir");
    std::fs::write(
        base.join("cyclic.rbs"),
        "type loop_t = loop_t\n\
         type a_t = b_t\n\
         type b_t = a_t\n\
         interface _Spinner\n  def spin: () -> Integer\n  def spin: () -> Integer\nend\n",
    )
    .expect("write sig");

    let data = CoreData::load_for_project(&[], std::slice::from_ref(&base));
    // No hang at ingestion; the self-cycle is retained as a raw Alias leaf.
    assert_eq!(
        data.resolve_type_alias("loop_t"),
        Some(&RetainedParamType::Alias("loop_t")),
        "self-referential alias retained one level deep, no loop"
    );
    assert_eq!(
        data.resolve_type_alias("a_t"),
        Some(&RetainedParamType::Alias("b_t")),
        "mutual alias retained raw (Slice 2 owns expansion)"
    );
    // Interface with a duplicate method decl dedups to one name.
    assert_eq!(
        data.interface_methods("_Spinner"),
        Some(["spin"].as_slice()),
        "duplicate interface method decls dedup"
    );
    let _ = std::fs::remove_dir_all(&base);
}

/// The retention is ADDITIVE: an empty `sig_dirs` load still has the same
/// class count, and the substrate accessors are populated (non-degenerate)
/// on the embedded set — a coarse guard that ingestion actually ran.
#[test]
fn atm_substrate_is_additive_and_populated() {
    let idx = CoreData::load();
    // The type-alias and interface tables are non-empty on the real RBS.
    assert!(idx.resolve_type_alias("string").is_some());
    assert!(idx.interface_methods("_ToStr").is_some());
    // A stdlib alias is present too (`type Pathname::glob_pattern` etc. vary;
    // assert the core `boolish` alias which builtin.rbs declares).
    assert!(
        idx.resolve_type_alias("boolish").is_some(),
        "core `type boolish` alias retained"
    );
}

// -- ATM Slice 2: acceptance-walk predicates ------------------------------

/// `ClassInstance` nil-admittance: the closed `NIL_COMPATIBLE` set admits,
/// every other concrete class rejects. `Object` (a universal nil ancestor)
/// admits; `String` does not — the load-bearing case (`"a" + nil` fires).
#[test]
fn atm_param_admits_nil_class_instance() {
    let idx = CoreData::load();
    assert!(idx.param_admits_nil(&RetainedParamType::ClassInstance("Object")));
    assert!(idx.param_admits_nil(&RetainedParamType::ClassInstance("BasicObject")));
    assert!(idx.param_admits_nil(&RetainedParamType::ClassInstance("Kernel")));
    assert!(idx.param_admits_nil(&RetainedParamType::ClassInstance("NilClass")));
    // A leading `::` is tolerated on the name.
    assert!(idx.param_admits_nil(&RetainedParamType::ClassInstance("::Object")));
    // Concrete classes reject nil.
    assert!(!idx.param_admits_nil(&RetainedParamType::ClassInstance("String")));
    assert!(!idx.param_admits_nil(&RetainedParamType::ClassInstance("Integer")));
}

/// `Other` forms all admit nil (the reference `else`: `nil`/`untyped`/`self`/
/// `bool`/literals/type variables …), and `Optional` always admits.
#[test]
fn atm_param_admits_nil_other_and_optional() {
    let idx = CoreData::load();
    for form in ["nil", "untyped", "self", "bool", "top", "void", "1", "\"x\"", ":sym", "T"] {
        assert!(
            idx.param_admits_nil(&RetainedParamType::Other(form.to_string())),
            "Other({form:?}) admits nil conservatively"
        );
    }
    assert!(idx.param_admits_nil(&RetainedParamType::Optional(Box::new(
        RetainedParamType::ClassInstance("String")
    ))));
}

/// `Union` admits nil iff ANY member does. `String | Integer` rejects (both
/// concrete non-nil); `String | Object` admits (via `Object`).
#[test]
fn atm_param_admits_nil_union() {
    let idx = CoreData::load();
    assert!(!idx.param_admits_nil(&RetainedParamType::Union(vec![
        RetainedParamType::ClassInstance("String"),
        RetainedParamType::ClassInstance("Integer"),
    ])));
    assert!(idx.param_admits_nil(&RetainedParamType::Union(vec![
        RetainedParamType::ClassInstance("String"),
        RetainedParamType::ClassInstance("Object"),
    ])));
}

/// The `string` / `int` interface-aliases reject nil: `type string = String
/// | _ToStr`, and `NilClass` implements neither `to_str` nor `to_int`, so
/// both arms reject. This is the semantic that makes `"a" + nil` fire
/// (proven live against the oracle). A hypothetical `_ToS`-shaped interface
/// admits, since `NilClass#to_s` exists.
#[test]
fn atm_param_admits_nil_string_int_aliases() {
    let idx = CoreData::load();
    assert!(
        !idx.param_admits_nil(&RetainedParamType::Alias("string")),
        "`string` (String | _ToStr) rejects nil — NilClass lacks to_str"
    );
    assert!(
        !idx.param_admits_nil(&RetainedParamType::Alias("int")),
        "`int` (Integer | _ToInt) rejects nil — NilClass lacks to_int"
    );
    // NilClass HAS to_s, so a `_ToS` interface param admits nil directly.
    assert!(
        idx.param_admits_nil(&RetainedParamType::Interface("_ToS")),
        "_ToS admits nil — NilClass#to_s exists"
    );
    // An unknown interface admits conservatively.
    assert!(idx.param_admits_nil(&RetainedParamType::Interface("_NoSuchIfaceZzz")));
}

/// `ClassInstance` argument acceptance via `class_ordering`: Equal / Subclass
/// / Superclass / Unknown accept, only a provable `Disjoint` rejects.
#[test]
fn atm_param_accepts_arg_class_instance() {
    let idx = CoreData::load();
    // Equal.
    assert!(idx.param_accepts_arg_class(&RetainedParamType::ClassInstance("String"), "String"));
    // Subclass: ArgumentError <: Exception.
    assert!(idx.param_accepts_arg_class(
        &RetainedParamType::ClassInstance("Exception"),
        "ArgumentError"
    ));
    // Superclass: arg Numeric is broader than param Integer — a runtime value
    // MIGHT be an Integer, so admit (never a provable reject).
    assert!(idx.param_accepts_arg_class(
        &RetainedParamType::ClassInstance("Integer"),
        "Numeric"
    ));
    // Unknown: an unloaded param class cannot be refuted.
    assert!(idx.param_accepts_arg_class(
        &RetainedParamType::ClassInstance("NoSuchClassZzz"),
        "String"
    ));
    // Disjoint: the sole rejection.
    assert!(!idx.param_accepts_arg_class(&RetainedParamType::ClassInstance("String"), "Symbol"));
    assert!(!idx.param_accepts_arg_class(
        &RetainedParamType::ClassInstance("Integer"),
        "String"
    ));
}

/// The `string` / `int` aliases accept their concrete arm directly and, via
/// the interface walk, decline to reject a class that implements the
/// conversion: `int` accepts `Float` because `Float` (over `Numeric`) has
/// `to_int`. `string` rejects `Symbol` (no `to_str`).
#[test]
fn atm_param_accepts_arg_string_int_aliases() {
    let idx = CoreData::load();
    // `string` accepts String (concrete arm, Equal).
    assert!(idx.param_accepts_arg_class(&RetainedParamType::Alias("string"), "String"));
    // `int` accepts Integer (concrete arm) AND Float (via _ToInt: Float has
    // Numeric#to_int) — the interface walk declining to reject a coercible.
    assert!(idx.param_accepts_arg_class(&RetainedParamType::Alias("int"), "Integer"));
    assert!(
        idx.param_accepts_arg_class(&RetainedParamType::Alias("int"), "Float"),
        "`int` accepts Float — Float implements to_int via Numeric"
    );
    // `string` rejects Symbol: Disjoint from String AND no to_str.
    assert!(
        !idx.param_accepts_arg_class(&RetainedParamType::Alias("string"), "Symbol"),
        "`string` rejects Symbol — not a String and no to_str"
    );
}

/// Union / Optional / Other acceptance: Union accepts iff any member does;
/// Optional and Other admit conservatively.
#[test]
fn atm_param_accepts_arg_union_optional_other() {
    let idx = CoreData::load();
    // Union: Symbol accepted by the Symbol arm though rejected by String.
    assert!(idx.param_accepts_arg_class(
        &RetainedParamType::Union(vec![
            RetainedParamType::ClassInstance("String"),
            RetainedParamType::ClassInstance("Symbol"),
        ]),
        "Symbol"
    ));
    // Union of two disjoint concretes rejects a third disjoint arg.
    assert!(!idx.param_accepts_arg_class(
        &RetainedParamType::Union(vec![
            RetainedParamType::ClassInstance("String"),
            RetainedParamType::ClassInstance("Symbol"),
        ]),
        "Integer"
    ));
    // Optional / Other admit unconditionally.
    assert!(idx.param_accepts_arg_class(
        &RetainedParamType::Optional(Box::new(RetainedParamType::ClassInstance("String"))),
        "Integer"
    ));
    assert!(idx.param_accepts_arg_class(&RetainedParamType::Other("untyped".to_string()), "Integer"));
}

/// Interface acceptance is conservative on the unknown side: an arg class not
/// RBS-known admits (it MIGHT implement the conversion via metaprogramming),
/// only a KNOWN class provably lacking a required method rejects.
#[test]
fn atm_interface_accepts_arg_unknown_side() {
    let idx = CoreData::load();
    // Unknown arg class → admit.
    assert!(idx.param_accepts_arg_class(
        &RetainedParamType::Interface("_ToStr"),
        "NoSuchClassZzz"
    ));
    // Unknown interface → admit.
    assert!(idx.param_accepts_arg_class(
        &RetainedParamType::Interface("_NoSuchIfaceZzz"),
        "Symbol"
    ));
    // Known arg class lacking the required method → reject.
    assert!(
        !idx.param_accepts_arg_class(&RetainedParamType::Interface("_ToStr"), "Symbol"),
        "_ToStr rejects Symbol — no to_str"
    );
}

/// Bounded alias expansion terminates on a cycle (returns conservative true
/// at the depth cap) and resolves a finite chain to its leaf. Uses a project
/// `sig/` dir since alias ingestion is parser-driven.
#[test]
fn atm_acceptance_alias_depth_cap_and_chain() {
    let base = std::env::temp_dir().join(format!("rigor-atm-s2-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).expect("temp sig dir");
    std::fs::write(
        base.join("chains.rbs"),
        "type a_t = b_t\n\
         type b_t = a_t\n\
         type int_t = Integer\n\
         type obj_t = Object\n",
    )
    .expect("write sig");
    let data = CoreData::load_for_project(&[], std::slice::from_ref(&base));

    // Cyclic alias: no hang, admits at the cap (conservative true).
    assert!(
        data.param_admits_nil(&RetainedParamType::Alias("a_t")),
        "cyclic alias terminates and admits at the depth cap"
    );
    assert!(data.param_accepts_arg_class(&RetainedParamType::Alias("a_t"), "String"));

    // Finite chain resolves to its leaf class.
    assert!(
        !data.param_admits_nil(&RetainedParamType::Alias("int_t")),
        "`type int_t = Integer` inherits Integer's nil rejection"
    );
    assert!(
        data.param_admits_nil(&RetainedParamType::Alias("obj_t")),
        "`type obj_t = Object` inherits Object's nil admittance"
    );
    assert!(data.param_accepts_arg_class(&RetainedParamType::Alias("int_t"), "Integer"));
    assert!(!data.param_accepts_arg_class(&RetainedParamType::Alias("int_t"), "String"));

    let _ = std::fs::remove_dir_all(&base);
}
