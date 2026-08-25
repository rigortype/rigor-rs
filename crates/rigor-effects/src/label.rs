//! The effect-label grammar and the subsumption relation over it.
//!
//! Ported from the reference's `lib/rigor/effects/label.rb` (ADR-103 WD1;
//! normative upstream in `docs/type-specification/effect-labels.md`).
//!
//! A label is a dot-path of lowercase segments — `io`, `io.net.http`,
//! `nondet.time`. The relation that matters is **segment-aware prefix
//! subsumption**: `io` admits `io.net.http` and rejects `iota`. Every function
//! here is pure and total; a malformed input is answered, never panicked on, so
//! a caller can ask [`valid`] and the rest in either order.

const SEPARATOR: char = '.';

/// Whether `label` is a well-formed label.
///
/// The grammar is `label = segment { "." segment }`, `segment = [a-z][a-z0-9]*`
/// — the reference's `Label::PATTERN` (`label.rb:16`), hand-matched rather than
/// compiled, so a crate whose whole point is having no dependencies does not
/// acquire `regex` for one production. Deliberately narrow: no underscores, no
/// hyphens, no uppercase, no empty segment, no trailing dot. Upstream anchors
/// with `\A`/`\z` and not `^`/`$` — an envelope reader must not accept a
/// smuggled newline — which falls out here because `\n` is not an admitted
/// byte in any position.
///
/// The reference additionally answers `false` for a non-String; Rust's type
/// system carries that, so the port takes `&str`.
#[must_use]
pub fn valid(label: &str) -> bool {
    if label.is_empty() {
        return false;
    }
    label.split(SEPARATOR).all(|segment| {
        let mut chars = segment.chars();
        match chars.next() {
            Some(first) if first.is_ascii_lowercase() => {}
            _ => return false,
        }
        chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
    })
}

/// The label's segments, outermost first: `"io.net.http"` -> `["io", "net",
/// "http"]`. A malformed label yields an empty vector rather than a partial
/// parse.
#[must_use]
pub fn segments(label: &str) -> Vec<&str> {
    if !valid(label) {
        return Vec::new();
    }
    label.split(SEPARATOR).collect()
}

/// Whether `bound` admits `label` under segment-aware prefix subsumption.
///
/// A label subsumes itself; `io` subsumes `io.net.http`; `io` does NOT subsume
/// `iota`, because the match is on segment boundaries and not on characters.
#[must_use]
pub fn subsumes(bound: &str, label: &str) -> bool {
    if !valid(bound) || !valid(label) {
        return false;
    }
    if bound == label {
        return true;
    }
    label.len() > bound.len()
        && label.starts_with(bound)
        && label.as_bytes()[bound.len()] == SEPARATOR as u8
}

/// The label one segment shallower, or `None` for a root (and for a malformed
/// label).
#[must_use]
pub fn parent(label: &str) -> Option<&str> {
    if !valid(label) {
        return None;
    }
    let index = label.rfind(SEPARATOR)?;
    Some(&label[..index])
}

/// The label's proper ancestors, outermost first and excluding the label
/// itself: `"io.net.http"` -> `["io", "io.net"]`. A root has no ancestors.
#[must_use]
pub fn ancestors(label: &str) -> Vec<&str> {
    let parts = segments(label);
    if parts.len() <= 1 {
        return Vec::new();
    }
    // Every ancestor is a prefix of `label`, so they are borrowed, not built.
    label
        .match_indices(SEPARATOR)
        .map(|(index, _)| &label[..index])
        .collect()
}

/// The label's outermost segment — the root whose ownership the registry
/// checks. `None` for a malformed label.
#[must_use]
pub fn root(label: &str) -> Option<&str> {
    segments(label).first().copied()
}

// ---------------------------------------------------------------------------
// Ported from the reference's `spec/rigor/effects/label_spec.rb`, case for
// case. This is ADR-0043 slice 1's named gate ("label subsumption
// unit-tested").
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_accepts_a_single_segment_and_a_dot_path() {
        assert!(valid("io"));
        assert!(valid("io.net.http"));
        assert!(valid("nondet.time"));
    }

    #[test]
    fn valid_accepts_digits_after_the_first_character_of_a_segment() {
        assert!(valid("io.s3"));
        assert!(valid("h2"));
    }

    #[test]
    fn valid_rejects_a_segment_not_starting_with_a_lowercase_letter() {
        for candidate in ["IO", "3io", "_io", ".io", "io..net", "io.", "io._net"] {
            assert!(!valid(candidate), "expected {candidate:?} to be rejected");
        }
    }

    #[test]
    fn valid_rejects_separators_the_grammar_does_not_have() {
        for candidate in ["io-net", "io_net", "io/net", "io net", "email:send"] {
            assert!(!valid(candidate), "expected {candidate:?} to be rejected");
        }
    }

    #[test]
    fn valid_rejects_the_empty_string() {
        assert!(!valid(""));
    }

    #[test]
    fn valid_rejects_a_multi_line_string_that_would_match_line_wise() {
        // `\A`/`\z` rather than `^`/`$`: an envelope reader must not accept a
        // smuggled newline.
        assert!(!valid("io\nrm -rf"));
    }

    #[test]
    fn subsumes_admits_a_descendant() {
        assert!(subsumes("io", "io.net.http"));
        assert!(subsumes("io.net", "io.net.http"));
    }

    #[test]
    fn subsumes_is_reflexive() {
        assert!(subsumes("io", "io"));
        assert!(subsumes("io.net.http", "io.net.http"));
    }

    #[test]
    fn subsumes_matches_on_segment_boundaries_not_on_characters() {
        // The whole point of the relation: a string-prefix test says true here.
        assert!(!subsumes("io", "iota"));
        assert!(!subsumes("cache", "cachet.read"));
        assert!(!subsumes("mutate.self", "mutate.selfish"));
    }

    #[test]
    fn subsumes_does_not_run_upwards() {
        assert!(!subsumes("io.net.http", "io.net"));
        assert!(!subsumes("io.net", "io"));
    }

    #[test]
    fn subsumes_answers_false_for_a_malformed_operand() {
        assert!(!subsumes("io", "IO.net"));
        assert!(!subsumes("", "io"));
    }

    #[test]
    fn segments_splits_outermost_first() {
        assert_eq!(segments("io.db.read"), ["io", "db", "read"]);
        assert_eq!(segments("io"), ["io"]);
    }

    #[test]
    fn segments_is_empty_for_a_malformed_label() {
        assert!(segments("io..net").is_empty());
        assert!(segments("").is_empty());
    }

    #[test]
    fn parent_drops_the_innermost_segment() {
        assert_eq!(parent("io.net.http"), Some("io.net"));
        assert_eq!(parent("io.net"), Some("io"));
    }

    #[test]
    fn parent_is_none_for_a_root_and_for_a_malformed_label() {
        assert_eq!(parent("io"), None);
        assert_eq!(parent("io..net"), None);
    }

    #[test]
    fn ancestors_lists_the_proper_ancestors_outermost_first() {
        assert_eq!(ancestors("io.net.http"), ["io", "io.net"]);
        assert_eq!(ancestors("io.net"), ["io"]);
    }

    #[test]
    fn ancestors_excludes_the_label_itself() {
        assert!(!ancestors("io.net.http").contains(&"io.net.http"));
    }

    #[test]
    fn ancestors_is_empty_for_a_root_and_for_a_malformed_label() {
        assert!(ancestors("io").is_empty());
        assert!(ancestors("io.").is_empty());
    }

    #[test]
    fn root_is_the_outermost_segment() {
        assert_eq!(root("io.db.read"), Some("io"));
        assert_eq!(root("telemetry"), Some("telemetry"));
    }

    #[test]
    fn root_is_none_for_a_malformed_label() {
        assert_eq!(root("Io"), None);
    }
}
