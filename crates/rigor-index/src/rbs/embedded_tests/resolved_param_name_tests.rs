use crate::rbs::CoreData;
use crate::rbs::RetainedParamType;

/// rigor-rs#811: the retained param name is the reference's
/// post-`resolve_type_names` spelling — the `expected:` label the oracle
/// renders. The bundled `wait` writes `Thread::Mutex` (namespace preserved,
/// not leaf-collapsed); `module Gem`'s `sources=` writes `SourceList`, which
/// resolves forward to `Gem::SourceList` even though `source_list.rbs`
/// ingests after `rubygems.rbs` (two-phase ingest).
#[test]
fn bundled_params_retain_resolved_names() {
    let idx = CoreData::load();
    let ovs = idx
        .method_overloads("ConditionVariable", "wait")
        .expect("ConditionVariable#wait");
    assert!(matches!(
        &ovs[0].required_positionals[0],
        RetainedParamType::ClassInstance(name, _) if *name == "Thread::Mutex"
    ));
    let ovs = idx
        .singleton_method_overloads("Gem", "sources=")
        .expect("Gem.sources=");
    assert!(matches!(
        &ovs[0].required_positionals[0],
        RetainedParamType::Optional(inner)
            if matches!(inner.as_ref(), RetainedParamType::ClassInstance(name, _) if *name == "Gem::SourceList")
    ));
}

/// rigor-rs#811: a `type`-alias / class-alias use resolves the same way —
/// `Mutex` (the bundled `class Mutex = Thread::Mutex` alias) normalises to
/// its target in a use site, exactly as `TypeNameResolver` chases
/// `aliases.fetch(head)`.
#[test]
fn lexical_resolution_chases_class_aliases() {
    let known: std::collections::HashSet<&'static str> =
        ["Thread", "Thread::Mutex", "Mutex", "W2"].into_iter().collect();
    let aliases: std::collections::HashMap<&'static str, (&'static str, Vec<&'static str>)> =
        [("Mutex", ("Thread::Mutex", Vec::new()))].into_iter().collect();
    // Bare `Mutex` in any scope → the alias target.
    assert_eq!(
        crate::rbs::resolve_lexical_name_in("Mutex", &["W2"], &known, &aliases),
        Some("Thread::Mutex")
    );
    // Tail positions chase too: `Mutex::B` → `Thread::Mutex::B` when
    // declared.
    let known2: std::collections::HashSet<&'static str> =
        ["Thread", "Thread::Mutex", "Thread::Mutex::B", "Mutex"].into_iter().collect();
    assert_eq!(
        crate::rbs::resolve_lexical_name_in("Mutex::B", &["W2"], &known2, &aliases),
        Some("Thread::Mutex::B")
    );
    // A cycle (`A = B; B = A`) stays on the alias name rather than looping.
    let known3: std::collections::HashSet<&'static str> = ["A", "B"].into_iter().collect();
    let aliases3: std::collections::HashMap<&'static str, (&'static str, Vec<&'static str>)> =
        [("A", ("B", Vec::new())), ("B", ("A", Vec::new()))].into_iter().collect();
    assert_eq!(
        crate::rbs::resolve_lexical_name_in("A", &[], &known3, &aliases3),
        Some("A")
    );
    // An alias whose target resolves nothing keeps the alias NAME
    // (`or return head`), so the label reads as the oracle's `::A` would.
    let aliases4: std::collections::HashMap<&'static str, (&'static str, Vec<&'static str>)> =
        [("A", ("Missing::Thing", Vec::new()))].into_iter().collect();
    assert_eq!(
        crate::rbs::resolve_lexical_name_in("A", &[], &known3, &aliases4),
        Some("A")
    );
}
