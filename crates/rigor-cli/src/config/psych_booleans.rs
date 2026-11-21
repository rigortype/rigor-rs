//! Which `severity:`-family scalars of a `.rigor.yml` the reference's Psych
//! reads as YAML 1.1 BOOLEANS where serde_yaml's 1.2 parse keeps a String
//! (issue #253). Psych resolves a PLAIN, untagged `off`/`on`/`no`/`yes` —
//! case-insensitively — to `false`/`true` at parse time, so
//! `severity_overrides: {call: off}` dies the reference's exit-64 `a YAML
//! boolean` death and `severity_profile: off` takes the uncaught `to_sym`
//! NoMethodError path, while serde_yaml hands the port a plain String and the
//! load silently succeeds. Quoted and `!!`-tagged spellings stay Strings on
//! both parsers, and `true`/`false` are booleans on both, so neither is
//! reported here.
//!
//! The serde visitor the config load drives cannot make the distinction: it
//! sees a scalar only after serde_yaml has resolved its style and tag — the
//! same blind spot the #490 `!!`-tagged KEY spellings hit. So this walks the
//! FIRST document's libyaml event stream directly (the same parser serde_yaml
//! drives and [`super::psych_error`] re-runs), replaying the DupOk mapping
//! rules — a direct key last-wins, a `<<` merge fills only unfilled slots, an
//! earlier merge beats a later one — so the scan names, per surviving entry of
//! the LAST top-level `severity_overrides:` mapping, the WRITTEN occurrence
//! that set it and whether that occurrence was a bare boolean.
//!
//! Anything the walk cannot name declines to `None`, which keeps serde's read:
//! a config the reference rejects that the port then accepts is a coverage
//! loss, the safe side for a diagnostic engine. (`read_yaml` has already died
//! on aliases and date-class scalars before this scan runs, so those shapes
//! never reach it.)

use std::collections::HashMap;
use std::ffi::CStr;
use std::mem::MaybeUninit;

use unsafe_libyaml::{
    yaml_event_delete, yaml_event_t, yaml_parser_delete, yaml_parser_initialize,
    yaml_parser_parse, yaml_parser_set_encoding, yaml_parser_set_input_string, yaml_event_type_t,
    YAML_ALIAS_EVENT, YAML_DOCUMENT_END_EVENT, YAML_MAPPING_END_EVENT, YAML_MAPPING_START_EVENT,
    YAML_PLAIN_SCALAR_STYLE, YAML_SCALAR_EVENT, YAML_SEQUENCE_END_EVENT,
    YAML_SEQUENCE_START_EVENT, YAML_STREAM_END_EVENT, YAML_UTF8_ENCODING,
};

/// The YAML 1.1 boolean a PLAIN scalar's text names, case-insensitively —
/// Psych's `ScalarScanner#tokenize` resolution, already mirrored for KEY
/// spellings by `psych_plain_to_s`: `off`/`no` → `false`, `on`/`yes` →
/// `true`. `true`/`false` need no port-side resolution (both parsers read
/// them as booleans) and `y`/`n` are Strings in both.
pub(super) fn bare_boolean_word(s: &str) -> Option<bool> {
    match s.to_ascii_lowercase().as_str() {
        "off" | "no" => Some(false),
        "on" | "yes" => Some(true),
        _ => None,
    }
}

/// The Psych boolean resolution of the `severity:` scalars one `.rigor.yml`
/// document writes. Only the LAST top-level occurrence of each key counts
/// (Psych's duplicate-key rule, the same last-wins the key-spelling collector
/// for the audit honours).
#[derive(Default)]
pub(super) struct PsychBooleans {
    /// Per surviving entry of the LAST top-level `severity_overrides:`
    /// mapping: key text → the boolean its WINNING occurrence resolves to.
    /// `None` (present or absent) means "not a bare boolean — keep serde's
    /// read", which covers quoted, tagged, aliased and complex-key shapes.
    pub(super) severity_overrides: HashMap<String, Option<bool>>,
    /// The LAST top-level `severity_profile:` scalar's Psych boolean.
    pub(super) severity_profile: Option<bool>,
}

/// Scan `text` (the config file's bytes, BOM-stripped as `read_yaml` strips
/// it) for the severity scalars Psych would read as booleans. The first
/// document only, matching both parsers. A libyaml failure returns the empty
/// scan — `read_yaml` owns the parse-error surface, so a text this cannot
/// parse has already died there.
pub(super) fn scan(text: &str) -> PsychBooleans {
    if !(text.contains("severity_overrides") || text.contains("severity_profile")) {
        return PsychBooleans::default();
    }
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut out = PsychBooleans::default();
    // Boxed: `yaml_parser_set_input_string` stores a pointer to the parser in
    // the parser itself (`read_handler_data`), so it must not move.
    let mut parser = Box::new(MaybeUninit::<unsafe_libyaml::yaml_parser_t>::uninit());
    // SAFETY: mirrors `psych_error::first_document_error`'s parser driving:
    // the parser is initialized before use and deleted exactly once, `text`
    // outlives it, each event is deleted after it is read, and the scalar
    // strings are copied out before the delete.
    unsafe {
        let parser = parser.as_mut_ptr();
        if yaml_parser_initialize(parser).fail {
            return out;
        }
        yaml_parser_set_encoding(parser, YAML_UTF8_ENCODING);
        yaml_parser_set_input_string(parser, text.as_ptr(), text.len() as u64);
        walk(parser, &mut out);
        yaml_parser_delete(parser);
    }
    out
}

/// One libyaml event, reduced to what the scan needs. The event is deleted
/// before this returns, so every payload is copied out first.
///
/// # Safety
/// `parser` is an initialized libyaml parser with a `set_input_string` source.
unsafe fn next_event(parser: *mut unsafe_libyaml::yaml_parser_t) -> Option<Ev> {
    let mut event = MaybeUninit::<yaml_event_t>::uninit();
    // SAFETY: the caller's contract — the parser is initialized and fed the
    // config text; the event is deleted before the payload's copies outlive it.
    unsafe {
        if yaml_parser_parse(parser, event.as_mut_ptr()).fail {
            return None;
        }
        let kind: yaml_event_type_t = (*event.as_ptr()).type_;
        let out = match kind {
            YAML_SCALAR_EVENT => {
                let data = (*event.as_ptr()).data.scalar;
                Ev::Scalar {
                    text: scalar_text(data.value),
                    plain: matches!(data.style, YAML_PLAIN_SCALAR_STYLE),
                    tagged: !data.tag.is_null(),
                }
            }
            YAML_ALIAS_EVENT => Ev::Alias,
            YAML_MAPPING_START_EVENT => Ev::MapStart,
            YAML_MAPPING_END_EVENT => Ev::MapEnd,
            YAML_SEQUENCE_START_EVENT => Ev::SeqStart,
            YAML_SEQUENCE_END_EVENT => Ev::SeqEnd,
            YAML_DOCUMENT_END_EVENT | YAML_STREAM_END_EVENT => Ev::End,
            _ => Ev::Other,
        };
        yaml_event_delete(event.as_mut_ptr());
        Some(out)
    }
}

/// A scalar event's text — libyaml NUL-terminates its buffers, and the config
/// text was read as UTF-8 before the scan ran.
///
/// # Safety
/// `ptr` is NULL or a NUL-terminated byte string.
unsafe fn scalar_text(ptr: *mut u8) -> String {
    if ptr.is_null() {
        return String::new();
    }
    // SAFETY: the caller's contract.
    unsafe { CStr::from_ptr(ptr.cast()) }.to_string_lossy().into_owned()
}

#[derive(Debug)]
enum Ev {
    /// A scalar's copied-out payload: the text, whether it was written PLAIN
    /// (unquoted), and whether it carried a tag.
    Scalar { text: String, plain: bool, tagged: bool },
    Alias,
    MapStart,
    MapEnd,
    SeqStart,
    SeqEnd,
    /// Document or stream end — stop; the scan reads the first document only.
    End,
    /// Stream/document start and everything the walk does not consume itself.
    Other,
}

/// The bare-boolean a PLAIN, untagged scalar resolves to, `None` for every
/// other shape (quoted, tagged, aliased — declines).
fn bare(plain: bool, tagged: bool, text: &str) -> Option<bool> {
    (!tagged && plain).then(|| bare_boolean_word(text)).flatten()
}

/// Consume the stream up to the root mapping, then walk its entries,
/// collecting the `severity:`-family scalars.
///
/// # Safety
/// `parser` is an initialized libyaml parser with a `set_input_string` source.
unsafe fn walk(parser: *mut unsafe_libyaml::yaml_parser_t, out: &mut PsychBooleans) {
    // SAFETY: every call below is the `next_event` contract — the parser is
    // initialized and fed the config text.
    unsafe {
        loop {
            match next_event(parser) {
                Some(Ev::MapStart) => break, // the root mapping
                // A scalar or sequence root never reaches a coercion with a
                // `severity:` key in it — `read_yaml` owns that death.
                Some(Ev::End) | None => return,
                _ => continue,
            }
        }
        loop {
            match next_event(parser) {
                Some(Ev::MapEnd) | None => return,
                Some(Ev::Scalar { text: key, .. }) => match (key.as_str(), next_event(parser)) {
                    // The LAST top-level `severity_overrides:` mapping counts;
                    // any other shape for the key clears the record, exactly as
                    // the key-spelling collector clears on each occurrence.
                    ("severity_overrides", Some(Ev::MapStart)) => {
                        out.severity_overrides.clear();
                        resolve_map(parser, &mut out.severity_overrides);
                    }
                    ("severity_overrides", _) => {
                        out.severity_overrides.clear();
                        skip_value(parser);
                    }
                    ("severity_profile", Some(Ev::Scalar { text, plain, tagged })) => {
                        out.severity_profile = bare(plain, tagged, &text);
                    }
                    ("severity_profile", _) => {
                        out.severity_profile = None;
                        skip_value(parser);
                    }
                    (_, Some(Ev::MapStart)) => skip_collection(parser),
                    (_, Some(Ev::SeqStart)) => skip_collection(parser),
                    _ => {}
                },
                // A complex or aliased key cannot be named — skip its value too.
                Some(Ev::MapStart) => {
                    skip_collection(parser);
                    skip_value(parser);
                }
                Some(Ev::SeqStart) => {
                    skip_collection(parser);
                    skip_value(parser);
                }
                Some(Ev::Alias) => skip_value(parser),
                _ => return,
            }
        }
    }
}

/// Replay the DupOk mapping rules over one mapping's events (its `MapStart`
/// already consumed, through the matching `MapEnd`), recording for every slot
/// the bare-boolean of the occurrence that last SET it: a direct key
/// last-wins, a `<<` merge (mapping, or a sequence whose every element is a
/// mapping) fills only slots not already taken, and a non-mergeable `<<`
/// stays a literal `"<<"` entry.
///
/// # Safety
/// `parser` is an initialized libyaml parser with a `set_input_string` source,
/// positioned inside a mapping.
unsafe fn resolve_map(
    parser: *mut unsafe_libyaml::yaml_parser_t,
    slots: &mut HashMap<String, Option<bool>>,
) {
    // SAFETY: every call below is the `next_event` contract — the parser is
    // initialized and fed the config text, positioned inside a mapping.
    unsafe {
        loop {
            match next_event(parser) {
                Some(Ev::MapEnd) | None => return,
                Some(Ev::Scalar { text: key, .. }) => match next_event(parser) {
                    Some(Ev::Scalar { text, plain, tagged }) => {
                        slots.insert(key, bare(plain, tagged, &text));
                    }
                    Some(Ev::MapStart) => {
                        if key == "<<" {
                            let mut merged = HashMap::new();
                            resolve_map(parser, &mut merged);
                            for (k, v) in merged {
                                slots.entry(k).or_insert(v);
                            }
                        } else {
                            // A compound value under a normal key still SETS
                            // the slot (DupOk inserts the whole mapping as the
                            // value), so it must wipe an earlier bare boolean
                            // even though no boolean of its own is recorded.
                            slots.insert(key, None);
                            skip_collection(parser);
                        }
                    }
                    Some(Ev::SeqStart) => {
                        if key != "<<" {
                            // Same wipe for a sequence value: the entry keeps
                            // serde's read wholesale.
                            slots.insert(key, None);
                            skip_collection(parser);
                            continue;
                        }
                        // `<<: [{…}, {…}]` merges each element only when EVERY
                        // element is a mapping (DupOk's `all?` gate); any other
                        // element keeps `<<` as a literal key. Fills stage
                        // first and apply only when the gate holds.
                        let mut fills: Vec<(String, Option<bool>)> = Vec::new();
                        let mut literal = false;
                        loop {
                            match next_event(parser) {
                                Some(Ev::SeqEnd) => break,
                                Some(Ev::MapStart) => {
                                    let mut m = HashMap::new();
                                    resolve_map(parser, &mut m);
                                    fills.extend(m);
                                }
                                Some(Ev::SeqStart) => {
                                    literal = true;
                                    skip_collection(parser);
                                }
                                Some(Ev::Scalar { .. }) | Some(Ev::Alias) => literal = true,
                                _ => return,
                            }
                        }
                        if literal {
                            slots.insert("<<".to_string(), None);
                        } else {
                            for (k, v) in fills {
                                slots.entry(k).or_insert(v);
                            }
                        }
                    }
                    Some(Ev::Alias) => {
                        // `read_yaml` dies on aliases before the scan runs; kept
                        // for the scan's standalone safety.
                        slots.insert(key, None);
                    }
                    _ => return,
                },
                // A complex or aliased key cannot be named — skip its value too.
                Some(Ev::MapStart) => {
                    skip_collection(parser);
                    skip_value(parser);
                }
                Some(Ev::SeqStart) => {
                    skip_collection(parser);
                    skip_value(parser);
                }
                Some(Ev::Alias) => skip_value(parser),
                _ => return,
            }
        }
    }
}

/// Consume events through the END matching a collection start that was
/// already consumed (mapping or sequence, nested either way).
///
/// # Safety
/// `parser` is an initialized libyaml parser with a `set_input_string` source,
/// positioned inside a collection.
unsafe fn skip_collection(parser: *mut unsafe_libyaml::yaml_parser_t) {
    // SAFETY: the `next_event` contract — the parser is initialized and fed
    // the config text, positioned inside a collection.
    unsafe {
        let mut depth = 1usize;
        loop {
            match next_event(parser) {
                Some(Ev::MapStart) | Some(Ev::SeqStart) => depth += 1,
                Some(Ev::MapEnd) | Some(Ev::SeqEnd) => {
                    depth -= 1;
                    if depth == 0 {
                        return;
                    }
                }
                None | Some(Ev::End) => return,
                _ => {}
            }
        }
    }
}

/// Consume one already-started compound value, or nothing for a scalar.
///
/// # Safety
/// `parser` is an initialized libyaml parser with a `set_input_string` source.
unsafe fn skip_value(parser: *mut unsafe_libyaml::yaml_parser_t) {
    // SAFETY: the `next_event` contract — see [`skip_collection`].
    unsafe {
        match next_event(parser) {
            Some(Ev::MapStart) | Some(Ev::SeqStart) => skip_collection(parser),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn overrides(text: &str) -> HashMap<String, Option<bool>> {
        scan(text).severity_overrides
    }

    #[test]
    fn bare_boolean_values_are_reported_case_insensitively() {
        for (word, b) in [
            ("off", false),
            ("OFF", false),
            ("Off", false),
            ("no", false),
            ("on", true),
            ("YES", true),
        ] {
            assert_eq!(
                overrides(&format!("severity_overrides:\n  call: {word}\n")),
                HashMap::from([("call".to_string(), Some(b))]),
                "bare `{word}` must resolve to {b}"
            );
        }
    }

    #[test]
    fn quoted_and_tagged_spellings_decline() {
        // Quoted: a String on both parsers.
        assert_eq!(
            overrides("severity_overrides:\n  call: \"off\"\n"),
            HashMap::from([("call".to_string(), None)])
        );
        // `!!str`: Psych's ToRuby keeps the String; serde_yaml agrees.
        assert_eq!(
            overrides("severity_overrides:\n  call: !!str off\n"),
            HashMap::from([("call".to_string(), None)])
        );
        // A non-boolean plain word is no Psych boolean either.
        assert_eq!(
            overrides("severity_overrides:\n  call: warning\n"),
            HashMap::from([("call".to_string(), None)])
        );
        // `y`/`n` are Strings in both parsers (Psych's boolean set is
        // off/on/yes/no/true/false, not YAML 1.1's wider one).
        assert_eq!(
            overrides("severity_overrides:\n  call: y\n"),
            HashMap::from([("call".to_string(), None)])
        );
    }

    #[test]
    fn flow_style_and_multiple_keys_report_per_key() {
        assert_eq!(
            overrides("severity_overrides: {call: off, call.undefined-method: on}\n"),
            HashMap::from([
                ("call".to_string(), Some(false)),
                ("call.undefined-method".to_string(), Some(true)),
            ])
        );
    }

    #[test]
    fn duplicate_keys_last_wins_like_psych() {
        assert_eq!(
            overrides("severity_overrides:\n  call: off\n  call: \"off\"\n"),
            HashMap::from([("call".to_string(), None)]),
            "the later quoted occurrence wins"
        );
        assert_eq!(
            overrides("severity_overrides:\n  call: \"off\"\n  call: off\n"),
            HashMap::from([("call".to_string(), Some(false))]),
            "the later bare occurrence wins"
        );
    }

    #[test]
    fn the_last_top_level_severity_overrides_mapping_counts() {
        // Psych keeps the last duplicate top-level key; an earlier mapping's
        // spellings must not name the entries that survived (#490 rule).
        let text = "severity_overrides:\n  call: off\nseverity_overrides:\n  dump.type: on\n";
        assert_eq!(
            overrides(text),
            HashMap::from([("dump.type".to_string(), Some(true))])
        );
        // A later NON-mapping occurrence clears the record entirely.
        assert_eq!(
            overrides("severity_overrides:\n  call: off\nseverity_overrides: 5\n")
                .is_empty(),
            true
        );
    }

    #[test]
    fn merge_keys_fill_only_unfilled_slots_in_order() {
        // Merge-source entries count.
        assert_eq!(
            overrides("severity_overrides:\n  <<: {call: off}\n"),
            HashMap::from([("call".to_string(), Some(false))])
        );
        // A direct key always wins over a merge, before or after it.
        assert_eq!(
            overrides("severity_overrides:\n  <<: {call: \"off\"}\n  call: off\n"),
            HashMap::from([("call".to_string(), Some(false))])
        );
        assert_eq!(
            overrides("severity_overrides:\n  call: off\n  <<: {call: \"off\"}\n"),
            HashMap::from([("call".to_string(), Some(false))])
        );
        // An all-mappings sequence merges each element, earlier wins.
        assert_eq!(
            overrides("severity_overrides:\n  <<:\n    - {call: off}\n    - {call: on}\n"),
            HashMap::from([("call".to_string(), Some(false))])
        );
        // A `<<` sequence with a non-mapping element keeps `<<` literal and
        // discards the staged fills (DupOk's `all?` gate).
        assert_eq!(
            overrides("severity_overrides:\n  <<:\n    - {call: off}\n    - 5\n"),
            HashMap::from([("<<".to_string(), None)])
        );
        // A non-mapping `<<` value is a literal entry, not a merge.
        assert_eq!(
            overrides("severity_overrides:\n  <<: off\n"),
            HashMap::from([("<<".to_string(), Some(false))])
        );
    }

    #[test]
    fn compound_values_and_complex_keys_decline() {
        // A nested mapping under a normal key keeps serde's read wholesale.
        assert_eq!(
            overrides("severity_overrides:\n  call:\n    nested: off\n"),
            HashMap::from([("call".to_string(), None)])
        );
        // ... and it WIPES an earlier bare boolean, being the later direct
        // occurrence (the slot exists with no boolean of its own).
        assert_eq!(
            overrides("severity_overrides:\n  call: off\n  call:\n    nested: x\n"),
            HashMap::from([("call".to_string(), None)])
        );
        // A sequence value under a normal key likewise — and it must not be
        // misread as a `<<` merge (no bogus `"<<"` slot).
        assert_eq!(
            overrides("severity_overrides:\n  call: [off]\n"),
            HashMap::from([("call".to_string(), None)])
        );
        assert_eq!(
            overrides("severity_overrides:\n  call: off\n  call: [x]\n"),
            HashMap::from([("call".to_string(), None)])
        );
        // A complex key cannot be named.
        assert_eq!(overrides("severity_overrides:\n  ? [a]\n  : off\n").is_empty(), true);
    }

    #[test]
    fn severity_profile_reports_the_last_scalar() {
        assert_eq!(scan("severity_profile: off\n").severity_profile, Some(false));
        assert_eq!(scan("severity_profile: On\n").severity_profile, Some(true));
        // Psych resolves the scalar; quoting keeps the String.
        assert_eq!(scan("severity_profile: \"off\"\n").severity_profile, None);
        assert_eq!(scan("severity_profile: strict\n").severity_profile, None);
        // Duplicate top-level keys: the last occurrence counts.
        assert_eq!(
            scan("severity_profile: off\nseverity_profile: strict\n").severity_profile,
            None
        );
        assert_eq!(
            scan("severity_profile: strict\nseverity_profile: off\n").severity_profile,
            Some(false)
        );
        // A compound value declines.
        assert_eq!(
            scan("severity_profile:\n  - off\n").severity_profile,
            None
        );
    }

    #[test]
    fn texts_without_severity_keys_short_circuit() {
        assert!(overrides("paths:\n  - app\n").is_empty());
        assert_eq!(scan("").severity_profile, None);
    }

    #[test]
    fn only_the_first_document_is_read() {
        // Both parsers load the first document; a boolean in a second one is
        // never resolved by either.
        assert_eq!(
            overrides("---\nseverity_overrides:\n  call: warning\n---\nseverity_overrides:\n  call: off\n"),
            HashMap::from([("call".to_string(), None)])
        );
    }

    #[test]
    fn a_bom_is_stripped_like_read_yaml_strips_it() {
        assert_eq!(
            overrides("\u{feff}severity_overrides:\n  call: off\n"),
            HashMap::from([("call".to_string(), Some(false))])
        );
    }
}

