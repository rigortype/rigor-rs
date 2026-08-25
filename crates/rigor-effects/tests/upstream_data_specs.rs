//! Upstream's two DATA specs, ported over the VENDORED bytes.
//!
//! This is layer 1 of the three-layer drift gate ADR-0043 slice 1 ships (the
//! other two are `harness/vendor_effects.py --check`, which needs the pinned
//! submodule, and the embedded-bytes sha256 assertion in `src/lib.rs`, which
//! does not).
//!
//! Sources, ported case for case at the `v0.3.4` pin (`b10bd5df`):
//!
//! - `spec/rigor/effects/registry_data_spec.rb`
//! - `spec/rigor/effects/catalog_data_spec.rb`
//!
//! These files are hand-written, not generated, and every row is a vocabulary
//! or audit decision — so a change to them must be a deliberate edit here too.
//! Six of the catalogue's 420 rows (1.4%) are exercised by
//! `harness/effects_diff.py`; these assertions are what covers the other 414.
//!
//! Two upstream cases are NOT ported, both for stated slice-1 carve-outs; each
//! is replaced by the strongest assertion the vendored data alone supports and
//! is marked `SLICE-1 DEVIATION` below.

use std::collections::BTreeSet;

use rigor_effects::{CORE_YML, REGISTRY_YML, catalog, label, registry};

// ===========================================================================
// registry_data_spec.rb — the shipped vocabulary
// ===========================================================================

/// Steins' v1 set, verbatim. Shared vocabulary: a policy naming these must read
/// the same against a PHP service and a Rails app.
const STEINS_V1: &[&str] = &[
    "exit",
    "ffi",
    "global.read",
    "global.write",
    "io",
    "io.db",
    "io.fs",
    "io.fs.read",
    "io.fs.write",
    "io.input",
    "io.ipc",
    "io.net",
    "io.net.http",
    "io.output",
    "io.output.buffer",
    "io.output.header",
    "io.output.stdout",
    "io.output.stderr",
    "io.process",
    "io.signal",
    "mutate",
    "mutate.local",
    "nondet",
    "nondet.random",
    "nondet.time",
];

/// Ruby's `mutate` leaves (Steins ADR-0055's names, ADR-103 WD14): no
/// `mutate.arg`.
const RUBY_LEAVES: &[&str] = &["mutate.self", "mutate.instance", "mutate.static"];

/// Proposed shared core leaves, to raise with Steins.
const PROPOSED_SHARED: &[&str] = &["io.db.read", "io.db.write", "io.db.transaction"];

/// The small shared set of application-meaning roots a policy actually names.
const APPLICATION_MEANING: &[&str] =
    &["telemetry", "email.send", "job.enqueue", "cache.read", "cache.write"];

#[test]
fn registry_carries_vocabulary_version_one() {
    assert_eq!(registry().vocabulary_version(), 1);
}

#[test]
fn registry_declares_exactly_the_four_groups_and_nothing_else() {
    let expected: BTreeSet<&str> = STEINS_V1
        .iter()
        .chain(RUBY_LEAVES)
        .chain(PROPOSED_SHARED)
        .chain(APPLICATION_MEANING)
        .copied()
        .collect();
    let declared: BTreeSet<&str> = registry().labels().iter().map(String::as_str).collect();

    assert_eq!(declared, expected);
    assert_eq!(declared.len(), 36);
}

#[test]
fn registry_carries_steins_v1_verbatim() {
    for label in STEINS_V1 {
        assert!(registry().labels().iter().any(|l| l == label), "{label:?} is not declared");
    }
}

#[test]
fn registry_spells_the_mutate_leaves_as_wd14_fixed_them() {
    for leaf in RUBY_LEAVES {
        assert!(registry().labels().iter().any(|l| l == leaf), "{leaf:?} is not declared");
    }
    assert!(!registry().labels().iter().any(|l| l == "mutate.arg"));
}

#[test]
fn registry_declares_every_label_in_the_grammar() {
    for declared in registry().labels() {
        assert!(label::valid(declared), "{declared:?} is not a well-formed label");
    }
}

#[test]
fn registry_ships_an_empty_retired_table_at_vocabulary_one() {
    // Nothing has been renamed or removed yet; an entry here without a version
    // bump would be a vocabulary-evolution violation.
    for declared in registry().labels() {
        assert_eq!(registry().retired(declared), None, "{declared:?}");
    }
    assert_eq!(registry().retired("io.sql"), None);
}

#[test]
fn registry_opens_only_the_roots_the_three_layers_name() {
    assert_eq!(registry().roots(), [
        "cache", "email", "exit", "ffi", "global", "io", "job", "mutate", "nondet", "telemetry"
    ]);
}

#[test]
fn registry_recognises_the_interior_nodes_a_bound_may_name() {
    for bound in
        ["io", "io.db", "io.fs", "io.net", "io.output", "mutate", "nondet", "email", "job", "cache"]
    {
        assert!(registry().known(bound), "expected {bound:?} to be a recognised bound");
    }
}

#[test]
fn registry_does_not_recognise_a_plausible_label_nobody_registered() {
    assert!(!registry().known("io.smtp"));
    assert!(!registry().known("rails.activejob.enqueue"));
}

#[test]
fn registry_suggests_a_registered_spelling_for_a_near_miss() {
    assert_eq!(registry().suggest("nondet.tim"), Some("nondet.time"));
    assert_eq!(registry().suggest("io.fs.writ"), Some("io.fs.write"));
}

#[test]
fn registry_is_embedded_in_the_binary() {
    // Upstream asserts the file is in the gemspec's `files` list, because
    // `Registry.default` degrades to an empty vocabulary when the data file is
    // missing and the omission would be silent at runtime. The port's analogue
    // is that the bytes are `include_str!`d — there is no packaging step that
    // can drop them — and that they are non-empty at the pin.
    assert!(REGISTRY_YML.contains("vocabulary: 1"));
    assert_eq!(REGISTRY_YML.lines().count(), 67);
    assert!(!registry().labels().is_empty());
}

// ===========================================================================
// catalog_data_spec.rb — the shipped core catalogue
// ===========================================================================

/// `(key, row)` over every row of the catalogue, both buckets, keyed exactly as
/// upstream's helper does: `Owner#selector` / `Owner.selector`.
fn rows() -> Vec<(String, &'static rigor_effects::Row)> {
    catalog()
        .class_names()
        .into_iter()
        .flat_map(|name| {
            let entry = catalog().class_entry(name).expect("listed");
            let instance = entry
                .instance_methods()
                .iter()
                .map(move |(selector, row)| (format!("{name}#{selector}"), row));
            let singleton = entry
                .singleton_methods()
                .iter()
                .map(move |(selector, row)| (format!("{name}.{selector}"), row));
            instance.chain(singleton)
        })
        .collect()
}

#[test]
fn catalog_loads_the_shipped_file_at_schema_one() {
    assert_eq!(catalog().schema(), 1);
    assert!(!catalog().class_names().is_empty());
}

/// The two vendored files move together or not at all: a `core.yml` audited
/// against a newer vocabulary than the `registry.yml` beside it is a half
/// re-vendor, which is precisely what the `--check` gate exists to prevent and
/// what a hand-copy of one file would produce.
#[test]
fn the_two_vendored_files_agree_on_the_vocabulary() {
    assert_eq!(catalog().vocabulary(), Some(registry().vocabulary_version()));
}

/// **The wholesale gate** (the mini-spec's semantics item 4). Every label the
/// catalogue can emit — from all 420 rows, from every class's instance and
/// singleton posture, and from every `defaults:` posture — must be in the
/// grammar AND recognised by the registry.
///
/// This is what catches a bad vendor or a bad ancestor rule in one assertion,
/// and it is the assertion the `known?`-with-ancestors trap breaks: `core.yml`'s
/// `global` posture emits the bare `global`, which is NOT one of the 36 declared
/// rows. See [`catalog_emits_exactly_one_label_no_row_declares`].
#[test]
fn catalog_spells_every_label_in_the_grammar_and_in_the_shared_vocabulary() {
    let mut seen = 0usize;
    for label in every_emitted_label() {
        seen += 1;
        assert!(label::valid(&label), "{label:?} is not a well-formed label");
        assert!(registry().known(&label), "{label:?} is not in the shared vocabulary");
    }
    assert_eq!(seen, 22, "the distinct-label count the slice-1 probe measured");

    // …and the sweep really did walk all 420 rows.
    assert_eq!(rows().len(), 420);
}

/// Every distinct label the catalogue can put on a summary.
fn every_emitted_label() -> BTreeSet<String> {
    let mut labels: BTreeSet<String> = BTreeSet::new();
    for (_, row) in rows() {
        labels.extend(row.labels().iter().cloned());
    }
    for name in catalog().class_names() {
        let entry = catalog().class_entry(name).expect("listed");
        labels.extend(entry.posture_labels().iter().cloned());
        labels.extend(entry.singleton_posture_labels().iter().cloned());
    }
    // Every `defaults:` posture, including any no class currently names.
    for posture in
        ["value", "world", "fs", "net", "ipc", "http", "process", "signal", "global", "nondet",
         "ffi", "stdout", "stderr", "stdin"]
    {
        labels.extend(
            catalog().posture_labels(posture).expect("declared").iter().cloned(),
        );
    }
    labels
}

/// THE trap, at the catalogue level: exactly one label the catalogue emits is
/// not a declared registry row. A port validating the catalogue against the 36
/// declared rows alone rejects the shipped catalogue.
#[test]
fn catalog_emits_exactly_one_label_no_row_declares() {
    let declared: BTreeSet<&str> = registry().labels().iter().map(String::as_str).collect();
    let implied_only: Vec<String> = every_emitted_label()
        .into_iter()
        .filter(|label| !declared.contains(label.as_str()))
        .collect();

    assert_eq!(implied_only, ["global"]);
    assert!(registry().known("global"), "the bare `global` must be a recognised bound");
}

/// The universal selectors are the third thing the catalogue answers with, and
/// they contribute NO label at all — answered as a row, not a posture, because
/// it IS a statement about the selector.
#[test]
fn catalog_answers_every_universal_selector_as_nothing() {
    assert_eq!(catalog().universal().len(), 34);
    for selector in catalog().universal() {
        for owner in ["IO", "Socket", "String"] {
            // `Kernel` deliberately excluded: it rows `freeze` / `dup` itself,
            // and a class's own row wins.
            let entry = catalog().lookup(owner, selector).expect("universal or posture");
            assert!(entry.labels().is_empty(), "{owner}#{selector} read as a labelled call");
        }
    }
}

#[test]
fn catalog_gives_every_class_a_why_and_a_posture_the_defaults_declare() {
    // A posture the `defaults:` table does not declare is refused at load, so
    // loading at all is the assertion; this pins that the table is actually
    // used rather than silently defaulting to ∅.
    assert_eq!(catalog().class_entry("IO").expect("listed").posture_labels(), ["io"]);
    assert!(catalog().class_entry("String").expect("listed").posture_labels().is_empty());

    for name in catalog().class_names() {
        let entry = catalog().class_entry(name).expect("listed");
        assert!(!entry.why().is_empty(), "{name} carries no `why:`");
        assert!(entry.posture().is_some(), "{name} names no posture");
    }
}

#[test]
fn catalog_gives_every_row_a_why() {
    for (key, row) in rows() {
        assert!(!row.why().is_empty(), "{key} carries no `why:`");
    }
}

#[test]
fn catalog_names_only_narrowing_handlers_narrowing_implements() {
    let narrowed: Vec<String> = rows()
        .into_iter()
        .filter_map(|(key, row)| {
            row.narrow().map(|handler| {
                assert!(
                    rigor_effects::catalog::NARROWING_HANDLERS.contains(&handler),
                    "{key} names {handler:?}"
                );
                key
            })
        })
        .collect();

    // Seven rows across six of the seven handlers; `sql_verb` has no `core.yml`
    // row at all and serves PLUGIN rows, which ADR-0043 puts out of scope.
    assert_eq!(narrowed.len(), 7);
}

/// A narrowed row's own `effects:` is the answer a caller with no call node
/// gets, so it must still be a sound upper bound rather than the ∅ an omitted
/// key would give. In slice 1 it is the ONLY answer, so this is load-bearing
/// twice over.
#[test]
fn catalog_gives_every_narrowed_row_a_non_empty_unnarrowed_fallback() {
    for (key, row) in rows() {
        if row.narrow().is_none() {
            continue;
        }
        assert!(!row.labels().is_empty(), "{key} narrows but has no unnarrowed fallback");
    }
}

/// **SLICE-1 DEVIATION.** Upstream asserts that every selector of
/// `MutationWidening::ARRAY_MUTATORS` (31) / `HASH_MUTATORS` (15) /
/// `MutationClassifier::STRING_MUTATORS` (26) answers
/// `mutates_receiver? == true`, which is what proves the YAML and the analyser's
/// own sets cannot drift. Those sets are Ruby CODE, not data — the internal spec
/// makes "the data file MUST NOT re-spell a selector list" normative — so the
/// port has nothing to compare against until slice 2 ports them.
///
/// What the vendored data alone supports, and what is asserted instead: the
/// three classes name the three sets, by the names upstream resolves, and no
/// other class names one.
#[test]
fn catalog_names_the_three_mutator_sets_by_reference() {
    let named: Vec<(&str, &str)> = catalog()
        .class_names()
        .into_iter()
        .filter_map(|name| {
            catalog().class_entry(name).expect("listed").mutator_set().map(|set| (name, set))
        })
        .collect();

    assert_eq!(named, [("Array", "array"), ("Hash", "hash"), ("String", "string")]);
    for (_, set) in named {
        assert!(rigor_effects::catalog::MUTATOR_SETS.contains(&set));
    }
    // The YAML never re-spells a selector list — the whole point of the
    // by-reference form. `push` is in `ARRAY_MUTATORS`; it has no row here.
    assert!(
        !catalog().class_entry("Array").expect("listed").instance_methods().contains_key("push")
    );
}

/// The single most important NEGATIVE invariant of the data file (ADR-103 WD3).
/// `data/builtins`' `purity:` facet answers fold-safety in the C-dispatch sense
/// — `Random#rand` is `leaf`, `Array#push` is `leaf` — and reading it as effect
/// freedom would be wrong in both directions.
///
/// Upstream asserts this over both the loader source and the data. The loader
/// half is structural here: this crate has no `data/builtins` to open and reads
/// nothing but its own two embedded strings — there is no filesystem access in
/// it at all.
#[test]
fn catalog_never_mentions_the_fold_safety_facet_in_the_data() {
    for (number, line) in CORE_YML.lines().enumerate() {
        assert!(
            !line.trim_start().starts_with("purity:"),
            "core.yml:{} spells the fold-safety facet as a key",
            number + 1
        );
    }
}

#[test]
fn catalog_gives_random_rand_a_nondet_random_label() {
    // Even though the fold catalogue calls it `leaf`.
    assert_eq!(catalog().lookup("Random", "rand").expect("rowed").labels(), ["nondet.random"]);
    assert!(catalog().lookup("Random", "rand").expect("rowed").mutates_receiver());
}

#[test]
fn catalog_is_embedded_in_the_binary() {
    // Upstream's gemspec-packaging assertion; see the registry twin.
    assert!(CORE_YML.contains("schema: 1"));
    assert_eq!(CORE_YML.lines().count(), 843);
    assert!(!catalog().class_names().is_empty());
}
