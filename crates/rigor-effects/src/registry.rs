//! The effect-label vocabulary: which spellings are recognised, which have been
//! retired, and which roots the shipped layers open.
//!
//! Ported from the reference's `lib/rigor/effects/registry.rb` (ADR-103 WD2)
//! over the vendored `vendor/effects/registry.yml`.
//!
//! **The load-bearing subtlety is [`Registry::known`]**: it answers the
//! declared rows UNION every ancestor of a declared row (`registry.rb:161`'s
//! `build_known`). Four of the ten roots — `global`, `email`, `job`, `cache` —
//! exist ONLY as implied ancestors; no row spells them. That is not academic:
//! `core.yml`'s `global` posture emits the bare `global` label, so a port that
//! validates the catalogue against the 36 declared rows alone **rejects the
//! shipped catalogue**. [`tests::known_admits_the_four_implied_roots`] pins it.
//!
//! Not ported in slice 1, deliberately: `Registry#with(labels:, owner:)` and
//! its root-ownership rule, which serve the project's `effects.labels:`
//! extension and the plugin layer — the declared lane (ADR-0043 slice 6). Slice
//! 1 needs the grammar, `known?` and `roots`.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;

use crate::Error;
use crate::label;

/// How far a misspelling may be from a known label before [`Registry::suggest`]
/// declines to guess (`registry.rb:36`). Two edits catches a transposition or a
/// dropped segment character without proposing an unrelated label for a
/// genuinely new spelling.
const SUGGESTION_DISTANCE_CAP: usize = 2;

/// A retired spelling maps to its replacements, which YAML may spell as a
/// scalar or as a list — the reference's `Array(replacements)`
/// (`registry.rb:168`). Empty at vocabulary 1, so this is the mechanism, not
/// data in use.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Replacements {
    One(String),
    Many(Vec<String>),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRegistry {
    #[serde(default)]
    vocabulary: u32,
    #[serde(default)]
    labels: Option<Vec<String>>,
    #[serde(default)]
    retired: Option<BTreeMap<String, Replacements>>,
}

/// The shipped vocabulary, as a frozen value object.
#[derive(Debug)]
pub struct Registry {
    vocabulary_version: u32,
    labels: Vec<String>,
    known: BTreeSet<String>,
    roots: Vec<String>,
    retired: BTreeMap<String, Vec<String>>,
}

impl Registry {
    /// Build a registry from registry-shaped YAML.
    ///
    /// The reference's `load_file` degrades a MISSING file to an empty
    /// vocabulary (fail-open for a bare install that opted data out) but a
    /// present-and-malformed file is a packaging bug and raises. The vendored
    /// bytes are never missing — they are `include_str!`d — so only the second
    /// half of that posture has anything to port, and it is this `Result`.
    ///
    /// # Errors
    ///
    /// [`Error::Yaml`] on unparseable YAML or an unexpected key.
    pub fn from_yaml_str(source: &str) -> Result<Self, Error> {
        let raw: RawRegistry = serde_yaml::from_str(source)?;
        Ok(Self::new(
            raw.vocabulary,
            raw.labels.unwrap_or_default(),
            raw.retired.unwrap_or_default(),
        ))
    }

    fn new(
        vocabulary_version: u32,
        labels: Vec<String>,
        retired: BTreeMap<String, Replacements>,
    ) -> Self {
        // `labels.map(&:to_s).uniq.sort` — a BTreeSet gives both at once.
        let labels: Vec<String> = labels.into_iter().collect::<BTreeSet<_>>().into_iter().collect();
        let known = build_known(&labels);
        let roots: Vec<String> = known
            .iter()
            .filter(|candidate| label::parent(candidate).is_none())
            .cloned()
            .collect();
        let retired = retired
            .into_iter()
            .map(|(spelling, replacements)| {
                let replacements = match replacements {
                    Replacements::One(one) => vec![one],
                    Replacements::Many(many) => many,
                };
                (spelling, replacements)
            })
            .collect();
        Self { vocabulary_version, labels, known, roots, retired }
    }

    /// The vocabulary version. Bumps only on a rename or a removal, never on a
    /// leaf addition.
    #[must_use]
    pub fn vocabulary_version(&self) -> u32 {
        self.vocabulary_version
    }

    /// Every DECLARED label, sorted. Implied ancestors (`email`, because
    /// `email.send` is declared) are recognised by [`Self::known`] but are not
    /// rows of the vocabulary and are not listed here.
    #[must_use]
    pub fn labels(&self) -> &[String] {
        &self.labels
    }

    /// The roots of the vocabulary — the outermost segments an extension would
    /// treat as already owned. Ten at vocabulary 1, four of them implied.
    #[must_use]
    pub fn roots(&self) -> &[String] {
        &self.roots
    }

    /// Whether the vocabulary recognises `label`: an exact row, **or an
    /// ancestor of one**. A bound may name an interior node the data file never
    /// spells out on its own line.
    #[must_use]
    pub fn known(&self, label: &str) -> bool {
        self.known.contains(label)
    }

    /// The replacement labels for a retired spelling, or `None` when the
    /// spelling was never retired. Empty at vocabulary 1: the mechanism is
    /// present so a snapshot written by a NEWER Rigor still reads.
    #[must_use]
    pub fn retired(&self, label: &str) -> Option<&[String]> {
        self.retired.get(label).map(Vec::as_slice)
    }

    /// The nearest recognised label to a misspelling, within
    /// [`SUGGESTION_DISTANCE_CAP`] edits, or `None` when nothing is close
    /// enough. A recognised label suggests nothing — ask [`Self::known`] first.
    #[must_use]
    pub fn suggest(&self, label: &str) -> Option<&str> {
        if !label::valid(label) || self.known(label) {
            return None;
        }
        let mut best = None;
        let mut best_distance = SUGGESTION_DISTANCE_CAP + 1;
        // `@known.each` upstream — the implied ancestors are candidates too.
        for candidate in &self.known {
            let distance = levenshtein(label, candidate, best_distance);
            if distance < best_distance {
                best = Some(candidate.as_str());
                best_distance = distance;
            }
        }
        best
    }
}

/// Declared rows UNION every ancestor of a declared row (`registry.rb:161`).
fn build_known(labels: &[String]) -> BTreeSet<String> {
    let mut known: BTreeSet<String> = labels.iter().cloned().collect();
    for declared in labels {
        for ancestor in label::ancestors(declared) {
            known.insert(ancestor.to_string());
        }
    }
    known
}

/// Bounded Levenshtein (`registry.rb:174`). Answers `cap + 1` — "further than
/// the cap" — as soon as the length difference or a whole row of the matrix
/// rules the candidate out, so scanning the vocabulary stays cheap.
fn levenshtein(from: &str, to: &str, cap: usize) -> usize {
    let beyond = cap + 1;
    let from: Vec<char> = from.chars().collect();
    let to: Vec<char> = to.chars().collect();
    if from.len().abs_diff(to.len()) > cap {
        return beyond;
    }
    let mut previous: Vec<usize> = (0..=to.len()).collect();
    for (row, char) in from.iter().enumerate() {
        let mut current = Vec::with_capacity(to.len() + 1);
        current.push(row + 1);
        for (column, other) in to.iter().enumerate() {
            let cost = usize::from(char != other);
            let value = (current[column] + 1)
                .min(previous[column + 1] + 1)
                .min(previous[column] + cost);
            current.push(value);
        }
        if current.iter().copied().min().unwrap_or(beyond) > cap {
            return beyond;
        }
        previous = current;
    }
    let last = previous[to.len()];
    if last > cap { beyond } else { last }
}

#[cfg(test)]
mod tests {
    use crate::registry;

    #[test]
    fn known_admits_a_declared_row() {
        assert!(registry().known("global.read"));
        assert!(registry().labels().iter().any(|l| l == "global.read"));
    }

    /// THE trap this slice exists to pin. `global`, `email`, `job` and `cache`
    /// are roots that no row spells; they are recognised only because
    /// `global.read` / `email.send` / `job.enqueue` / `cache.read` declare
    /// descendants. `core.yml`'s `global` posture emits the bare `global`, so a
    /// port validating against the 36 declared rows REJECTS the shipped
    /// catalogue. Measured against the pinned loader in the slice-1 probe § 1a.
    #[test]
    fn known_admits_the_four_implied_roots() {
        for implied in ["global", "email", "job", "cache"] {
            assert!(registry().known(implied), "{implied:?} must be a recognised bound");
            assert!(
                !registry().labels().iter().any(|l| l == implied),
                "{implied:?} must NOT be a declared row — it is implied only"
            );
        }
    }

    #[test]
    fn known_rejects_a_plausible_label_nobody_registered() {
        assert!(!registry().known("io.smtp"));
        assert!(!registry().known("rails.activejob.enqueue"));
        // Not a label at all.
        assert!(!registry().known("IO"));
    }

    #[test]
    fn suggest_offers_a_registered_spelling_for_a_near_miss() {
        assert_eq!(registry().suggest("nondet.tim"), Some("nondet.time"));
        assert_eq!(registry().suggest("io.fs.writ"), Some("io.fs.write"));
    }

    #[test]
    fn suggest_declines_for_a_recognised_or_distant_spelling() {
        assert_eq!(registry().suggest("io.fs.write"), None);
        assert_eq!(registry().suggest("completely.unrelated.spelling"), None);
        assert_eq!(registry().suggest("NOT A LABEL"), None);
    }

    #[test]
    fn retired_is_empty_at_vocabulary_one() {
        assert_eq!(registry().vocabulary_version(), 1);
        for declared in registry().labels() {
            assert_eq!(registry().retired(declared), None, "{declared:?}");
        }
        assert_eq!(registry().retired("io.sql"), None);
    }

    #[test]
    fn a_retired_scalar_and_a_retired_list_both_read() {
        // The mechanism, exercised on synthetic bytes — the shipped table is
        // empty, and an entry there without a `vocabulary:` bump would be a
        // vocabulary-evolution violation.
        let source = "vocabulary: 2\nlabels: [io.net]\nretired:\n  io.sql: io.db\n  io.old: [io.db.read, io.db.write]\n";
        let parsed = super::Registry::from_yaml_str(source).expect("parses");

        assert_eq!(parsed.retired("io.sql"), Some(["io.db".to_string()].as_slice()));
        assert_eq!(
            parsed.retired("io.old"),
            Some(["io.db.read".to_string(), "io.db.write".to_string()].as_slice())
        );
        assert_eq!(parsed.retired("io.net"), None);
    }

    #[test]
    fn an_unknown_key_is_refused() {
        // Deliberately STRICTER than upstream's tolerant loader: this tree is a
        // pin surface, and a key upstream ADDS must be a loud failure here
        // rather than a silently ignored one.
        let error = super::Registry::from_yaml_str("vocabulary: 1\nlabels: []\nnovel: 3\n");

        assert!(error.is_err(), "an unknown key must not load silently");
    }
}
