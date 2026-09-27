use super::*;

/// `ERB::Util` is registered under its full qualified key, and BOTH its
/// `self?.html_escape` singleton and instance halves are visible via the
/// new own-entry-only accessors (vendored
/// `stdlib/erb/0/erb.rbs`: `module ERB; module Util; def self?.html_escape`).
#[test]
fn erb_util_qualified_singleton_instance_both_true() {
    let idx = CoreData::load();
    if !idx.knows_class("ERB") {
        return; // stub fallback: qualified registry is empty, nothing to assert.
    }
    assert!(idx.knows_qualified_class("ERB::Util"));
    assert!(idx.qualified_declares_instance("ERB::Util", "html_escape"));
    assert!(idx.qualified_declares_singleton("ERB::Util", "html_escape"));
    assert!(!idx.qualified_declares_singleton("ERB::Util", "no_such_method"));
    assert!(!idx.qualified_declares_instance("ERB::Util", "no_such_method"));
}

/// The short-key MERGE collision (`ERB::Util` and `CGI::Util` both collapse
/// onto the shared short key `"Util"` in `classes`) is now SPLIT in the
/// qualified registry: both are known, DISTINCT entries — an ERB::Util-only
/// instance method (`html_escape`) is absent from `CGI::Util`, and a
/// CGI::Util-only instance method (`pretty`, vendored
/// `stdlib/cgi/0/core.rbs`: `module CGI; module Util; def pretty`) is absent
/// from `ERB::Util`.
#[test]
fn erb_util_and_cgi_util_are_distinct_qualified_entries() {
    let idx = CoreData::load();
    if !idx.knows_class("ERB") || !idx.knows_class("CGI") {
        return;
    }
    assert!(idx.knows_qualified_class("ERB::Util"));
    assert!(idx.knows_qualified_class("CGI::Util"));
    // ERB::Util-only method is not on CGI::Util.
    assert!(idx.qualified_declares_instance("ERB::Util", "html_escape"));
    assert!(!idx.qualified_declares_instance("CGI::Util", "html_escape"));
    // CGI::Util-only method is not on ERB::Util.
    assert!(idx.qualified_declares_instance("CGI::Util", "pretty"));
    assert!(!idx.qualified_declares_instance("ERB::Util", "pretty"));
}

/// `resolve_short_unambiguous` collapses to `None` for an AMBIGUOUS short
/// name (`"Util"` is shared by `ERB::Util` and `CGI::Util`), resolves a
/// genuinely-unique nested short name (`"DefMethod"`, only
/// `ERB::DefMethod` in the vendored set) to its single qualified key, and
/// is `None` for an unknown name.
#[test]
fn resolve_short_unambiguous_collapses_ambiguity() {
    let idx = CoreData::load();
    if !idx.knows_class("ERB") || !idx.knows_class("CGI") {
        return;
    }
    assert_eq!(idx.resolve_short_unambiguous("Util"), None);
    assert_eq!(
        idx.resolve_short_unambiguous("DefMethod"),
        Some("ERB::DefMethod")
    );
    assert_eq!(idx.resolve_short_unambiguous("NoSuchNameZZZ"), None);
}

/// A genuine top-level class round-trips: its qualified key equals its
/// short key (no enclosing, no own namespace).
#[test]
fn toplevel_class_qualified_equals_short() {
    let idx = CoreData::load();
    if !idx.knows_class("Time") {
        return;
    }
    assert!(idx.knows_qualified_class("Time"));
}

/// Regression guard: the EXISTING short-key `knows_class` API is
/// UNCHANGED by this slice — `"Util"` is still known there too (the
/// short-key map still holds the merged, collapsed-union composite
/// exactly as before). This documents that Slice 1 is purely additive.
#[test]
fn short_key_map_unchanged_still_knows_util() {
    let idx = CoreData::load();
    if !idx.knows_class("ERB") {
        return;
    }
    assert!(idx.knows_class("Util"));
}

/// S0 (2026-08-08): a decl at lexical depth ≥ 3 registers under its TRUE
/// qualified key, not a doubled one. `module Bundler; module Source; class
/// Git` (`overlay/vendored_gem_sigs/bundler/bundler.rbs`) used to register
/// as `Bundler::Bundler::Source::Git` because `qualified_name` joined the
/// whole scope CHAIN instead of taking its innermost element.
#[test]
fn depth_three_decl_is_not_double_prefixed() {
    let idx = CoreData::load();
    if !idx.knows_class("Bundler") {
        return;
    }
    assert!(idx.knows_qualified_class("Bundler::Source"));
    assert!(idx.knows_qualified_class("Bundler::Source::Git"));
    assert!(idx.knows_qualified_class("Bundler::Source::Rubygems"));
    assert!(!idx.knows_qualified_class("Bundler::Bundler::Source::Git"));
    assert!(!idx.knows_qualified_class("Bundler::Bundler::Source::Rubygems"));
    // The real entry carries the real surface, and witnesses absence.
    assert!(idx.qualified_declares_instance("Bundler::Source::Git", "initialize"));
    assert!(!idx.qualified_class_has_method("Bundler::Source::Git", "frobnicate_zzz"));
}

/// S0 controls: depth-2 nesting (`module URI; class HTTP`) and a
/// SELF-QUALIFIED file-level decl (`class Nokogiri::CSS::Parser`, whose
/// path rides its own `TypeNameNode` namespace) are unchanged — both were
/// already correct and must stay so.
#[test]
fn depth_two_and_self_qualified_decls_unchanged() {
    let idx = CoreData::load();
    if !idx.knows_class("URI") {
        return;
    }
    assert!(idx.knows_qualified_class("URI::HTTP"));
    assert!(idx.knows_qualified_class("URI::Generic"));
    assert!(!idx.knows_qualified_class("URI::URI::HTTP"));
    if idx.knows_class("Nokogiri") {
        assert!(idx.knows_qualified_class("Nokogiri::CSS::Parser"));
        assert!(!idx.knows_qualified_class("Nokogiri::Nokogiri::CSS::Parser"));
    }
}
