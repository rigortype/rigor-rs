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
    // A cycle (`A = B; B = A`) fails the whole name — `head = normalize or
    // return head` makes `head` falsy on a `visited` re-entry — so the use
    // site keeps the WRITTEN name (the caller's `unwrap_or`), as the
    // reference's unresolved name does.
    let known3: std::collections::HashSet<&'static str> = ["A", "B"].into_iter().collect();
    let aliases3: std::collections::HashMap<&'static str, (&'static str, Vec<&'static str>)> =
        [("A", ("B", Vec::new())), ("B", ("A", Vec::new()))].into_iter().collect();
    assert_eq!(
        crate::rbs::resolve_lexical_name_in("A", &[], &known3, &aliases3),
        None
    );
    // An alias whose target resolves nothing fails the same way.
    let aliases4: std::collections::HashMap<&'static str, (&'static str, Vec<&'static str>)> =
        [("A", ("Missing::Thing", Vec::new()))].into_iter().collect();
    assert_eq!(
        crate::rbs::resolve_lexical_name_in("A", &[], &known3, &aliases4),
        None
    );
    // An alias in the TAIL position chases too (`M::Mutex` ⇒ `Thread::Mutex`)
    // — `has_type_name?` misses alias-only names, so `aliases.fetch` takes it.
    let known4: std::collections::HashSet<&'static str> =
        ["M", "Thread", "Thread::Mutex"].into_iter().collect();
    let aliases5: std::collections::HashMap<&'static str, (&'static str, Vec<&'static str>)> =
        [("M::Mutex", ("Thread::Mutex", Vec::new()))].into_iter().collect();
    assert_eq!(
        crate::rbs::resolve_lexical_name_in("M::Mutex", &[], &known4, &aliases5),
        Some("Thread::Mutex")
    );
}
