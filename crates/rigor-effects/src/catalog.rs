//! The built-in effect catalogue — which core-library methods colour a summary,
//! and with what.
//!
//! Ported from the reference's `lib/rigor/effects/catalog.rb` (ADR-103 WD3 /
//! WD14) over the vendored `vendor/effects/core.yml`.
//!
//! **The loader is deliberately dumb.** Every audit decision lives in the data
//! file's `why:` lines; nothing here decides what a method does. What this adds
//! over a plain YAML parse is three things the data cannot express on its own —
//! and slice 1 carries two of the three:
//!
//! 1. **Per-class default postures.** A class the catalogue LISTS answers its
//!    posture's labels for a method it does not row; a class the catalogue does
//!    NOT list answers `None` — contribute nothing, do not taint — which stays
//!    the reading for project and gem classes.
//! 2. **The lookup PRECEDENCE**: the class's own row → the 34-name `universal:`
//!    list → the class's posture (`catalog.rb:184`). Precedence is mechanical
//!    lookup order and is testable now.
//! 3. Argument-dependent narrowing and mutator-set expansion, which are
//!    slice-2 CODE. See the crate docs' carve-out list: `narrow:` and
//!    `mutators:` are carried as OPAQUE NAMES, validated to exist and otherwise
//!    untouched.
//!
//! What this is **not** is a re-reading of `data/builtins/ruby_core/*.yml`'s
//! `purity:` facet. That facet answers fold-safety in the C-dispatch sense —
//! `Random#rand` is `leaf`, `Array#push` is `leaf` — and reading it as effect
//! freedom would be wrong in both directions (ADR-103 WD3). This loader never
//! opens anything but its own bytes, and a test asserts the data file never
//! spells the key.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;

use crate::Error;
use crate::label;

/// The narrowing handlers upstream's `Effects::Narrowing::HANDLERS` implements
/// (`lib/rigor/effects/narrowing.rb:55`), vendored as NAMES only.
///
/// Slice 1 validates a row's `narrow:` against this list — upstream rejects an
/// unknown handler at load time rather than at call time — and implements none
/// of the bodies. `core.yml` uses six of the seven across seven rows;
/// `sql_verb` has no `core.yml` row at all and serves PLUGIN rows, which
/// ADR-0043 puts out of scope.
pub const NARROWING_HANDLERS: &[&str] = &[
    "kernel_open",
    "file_open",
    "pathname_open",
    "time_new",
    "random_new",
    "uri_open",
    "sql_verb",
];

/// The mutator sets a class may name, by reference (upstream's
/// `Catalog::MUTATOR_SETS`, `catalog.rb:43`). The internal spec makes the
/// by-reference rule normative — "The data file MUST NOT re-spell a selector
/// list" — so `core.yml` carries only these names and the selectors arrive
/// through [`crate::mutators`], extracted from the pinned Ruby.
pub const MUTATOR_SETS: &[&str] = &["array", "hash", "string"];

// ---------------------------------------------------------------------------
// The raw shapes, as the YAML spells them.
//
// `deny_unknown_fields` is DELIBERATELY stricter than upstream's tolerant
// loader. This tree is a pin surface whose whole slice-1 job is to detect
// drift; a key upstream ADDS must be a loud `cargo test` failure here, not a
// silently ignored one. There is no behaviour risk: nothing consumes the crate,
// and every key present at the pin parses.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCatalog {
    #[serde(default)]
    schema: u32,
    /// Which registry vocabulary this catalogue was audited against. Upstream's
    /// `Catalog` ignores the key (its `Registry` twin carries the authority);
    /// the port keeps it, because the two files moving out of step is exactly
    /// the drift this tree exists to catch.
    #[serde(default)]
    vocabulary: Option<u32>,
    #[serde(default)]
    defaults: BTreeMap<String, Option<Vec<String>>>,
    #[serde(default)]
    universal: Vec<String>,
    #[serde(default)]
    classes: BTreeMap<String, Option<RawClass>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawClass {
    #[serde(default)]
    posture: Option<String>,
    #[serde(default)]
    singleton_posture: Option<String>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    mutators: Option<String>,
    #[serde(default)]
    why: Option<String>,
    #[serde(default)]
    methods: Option<BTreeMap<String, Option<RawRow>>>,
    #[serde(default)]
    singleton_methods: Option<BTreeMap<String, Option<RawRow>>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRow {
    #[serde(default)]
    effects: Option<Vec<String>>,
    #[serde(default)]
    narrow: Option<String>,
    #[serde(default)]
    mutates: Option<String>,
    #[serde(default)]
    why: Option<String>,
}

// ---------------------------------------------------------------------------
// The resolved shapes.
// ---------------------------------------------------------------------------

/// What the catalogue answers for one call.
///
/// `labels` may be empty two ways, and **the difference matters**: an explicit
/// ∅ **row** says the catalogue knows this call and knows it contributes
/// nothing (which is what stops `Thread.new` reading as an unresolved call
/// while its block joins the enclosing method by containment), while a ∅
/// **posture** says only that the class is a value class. Both stop the caller
/// treating the call as unresolved; only the posture keeps a project edge,
/// because a project may reopen a core class — which is why [`Entry::posture`]
/// is carried rather than inferred from emptiness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    labels: Vec<String>,
    mutates_receiver: bool,
    from_posture: bool,
}

impl Entry {
    /// The labels this call contributes, sorted and de-duplicated (the
    /// normalisation upstream's `LabelSet.new` performs).
    #[must_use]
    pub fn labels(&self) -> &[String] {
        &self.labels
    }

    /// Whether this call is a receiver mutation on the catalogue's own reading
    /// — the row's `mutates: receiver`, or (either on a row or on the posture)
    /// the selector being a member of the class's `mutators:` set.
    #[must_use]
    pub fn mutates_receiver(&self) -> bool {
        self.mutates_receiver
    }

    /// Whether this answer came from the class's posture rather than from a row
    /// or the universal list.
    #[must_use]
    pub fn posture(&self) -> bool {
        self.from_posture
    }
}

/// One row of the data file, resolved.
#[derive(Debug, Clone)]
pub struct Row {
    narrow: Option<String>,
    why: String,
    entry: Entry,
}

impl Row {
    /// The row's own labels — for a narrowed row, its **unnarrowed** answer:
    /// the upper bound the handler degrades to.
    ///
    /// **A slice-2 consumer holding the call node must branch on
    /// [`Self::narrow`] before reading this** (ADR-0043 § 2 grades the proven
    /// lane as a raw STRING-set subset, so `io.fs` where the oracle proves
    /// `io.fs.read` is an OVER, not a coarser truth).
    #[must_use]
    pub fn labels(&self) -> &[String] {
        self.entry.labels()
    }

    /// The `narrow:` handler NAME, or `None`. Opaque in slice 1; validated to
    /// be one of [`NARROWING_HANDLERS`] at load.
    #[must_use]
    pub fn narrow(&self) -> Option<&str> {
        self.narrow.as_deref()
    }

    /// Whether the row declares `mutates: receiver`.
    #[must_use]
    pub fn mutates_receiver(&self) -> bool {
        self.entry.mutates_receiver()
    }

    /// The row's `why:` — the audit decision. Every row has one; upstream
    /// refuses to load a row that does not.
    #[must_use]
    pub fn why(&self) -> &str {
        &self.why
    }

    /// The row's unnarrowed [`Entry`], built at load time (upstream allocates
    /// it once too — the lookup sits inside the effect scan's walk).
    #[must_use]
    pub fn entry(&self) -> &Entry {
        &self.entry
    }
}

/// One class of the data file, resolved: its two method buckets, its posture's
/// labels, its mutator set (name AND expansion), and the three memoised posture
/// [`Entry`]s.
///
/// Three, not two, for the reason upstream's `posture_entry(singleton,
/// mutating)` memoises three (`catalog.rb:88-97`): only the instance side can be
/// a receiver mutation, so the singleton side needs one entry and the instance
/// side needs a mutating and a non-mutating one.
#[derive(Debug)]
pub struct ClassEntry {
    instance_methods: BTreeMap<String, Row>,
    singleton_methods: BTreeMap<String, Row>,
    posture: Option<String>,
    posture_labels: Vec<String>,
    singleton_posture_labels: Vec<String>,
    mutators: Option<String>,
    mutator_selectors: BTreeSet<String>,
    object: bool,
    why: String,
    posture_entry: Entry,
    mutating_posture_entry: Entry,
    singleton_posture_entry: Entry,
}

impl ClassEntry {
    /// The `methods:` bucket, by selector.
    #[must_use]
    pub fn instance_methods(&self) -> &BTreeMap<String, Row> {
        &self.instance_methods
    }

    /// The `singleton_methods:` bucket, by selector.
    #[must_use]
    pub fn singleton_methods(&self) -> &BTreeMap<String, Row> {
        &self.singleton_methods
    }

    /// The `posture:` key this class names, or `None` (which reads as ∅).
    #[must_use]
    pub fn posture(&self) -> Option<&str> {
        self.posture.as_deref()
    }

    /// What an UNCATALOGUED instance method of this class contributes.
    #[must_use]
    pub fn posture_labels(&self) -> &[String] {
        &self.posture_labels
    }

    /// What an uncatalogued SINGLETON method contributes. Falls back to
    /// [`Self::posture_labels`] unless the class names its own
    /// `singleton_posture:` — `Kernel` is the one that does.
    #[must_use]
    pub fn singleton_posture_labels(&self) -> &[String] {
        &self.singleton_posture_labels
    }

    /// The `mutators:` set NAME this class references, or `None`.
    #[must_use]
    pub fn mutator_set(&self) -> Option<&str> {
        self.mutators.as_deref()
    }

    /// Whether `selector` is in this class's `mutators:` set — upstream's
    /// `entry.mutators.include?(name)` (`catalog.rb:194`), the second disjunct
    /// of both mutation questions. Always false for a class with no set.
    #[must_use]
    pub fn in_mutator_set(&self, selector: &str) -> bool {
        self.mutator_selectors.contains(selector)
    }

    /// Whether the constant names an OBJECT rather than a class, so a call on
    /// it spells `ENV#[]` and not `ENV.[]`.
    #[must_use]
    pub fn object(&self) -> bool {
        self.object
    }

    /// The class's `why:` — its audit decision.
    #[must_use]
    pub fn why(&self) -> &str {
        &self.why
    }
}

/// The resolved catalogue.
#[derive(Debug)]
pub struct Catalog {
    schema: u32,
    vocabulary: Option<u32>,
    digest: String,
    postures: BTreeMap<String, Vec<String>>,
    universal: BTreeSet<String>,
    classes: BTreeMap<String, ClassEntry>,
    object_constants: BTreeSet<String>,
}

/// An `Object`-level selector that exists on every receiver and touches
/// nothing. Answered as a ROW rather than a posture, because it IS a statement
/// about the selector (`catalog.rb:199`).
static UNIVERSAL_ENTRY: Entry =
    Entry { labels: Vec::new(), mutates_receiver: false, from_posture: false };

impl Catalog {
    /// Build a catalogue from catalogue-shaped YAML.
    ///
    /// # Errors
    ///
    /// Every load-time refusal upstream makes: a malformed document, a label
    /// outside the grammar, an unknown posture, an unknown mutator-set name, an
    /// unknown narrowing handler, or a row with no `why:`.
    pub fn from_yaml_str(source: &str) -> Result<Self, Error> {
        let raw: RawCatalog = serde_yaml::from_str(source)?;
        Self::build(raw, crate::sha256_hex(source.as_bytes()))
    }

    fn build(raw: RawCatalog, digest: String) -> Result<Self, Error> {
        let mut postures = BTreeMap::new();
        for (name, labels) in raw.defaults {
            postures.insert(name.clone(), label_set(&format!("defaults.{name}"), labels)?);
        }
        let universal: BTreeSet<String> = raw.universal.into_iter().collect();

        let mut classes = BTreeMap::new();
        for (name, body) in raw.classes {
            let entry = build_class(&name, body.unwrap_or(RawClass::empty()), &postures)?;
            classes.insert(name, entry);
        }
        let object_constants = classes
            .iter()
            .filter(|(_, entry)| entry.object)
            .map(|(name, _)| name.clone())
            .collect();

        Ok(Self {
            schema: raw.schema,
            vocabulary: raw.vocabulary,
            digest,
            postures,
            universal,
            classes,
            object_constants,
        })
    }

    /// The data file's `schema:`. Bumped for a shape change; a re-audited row
    /// moves [`Self::digest`] instead.
    #[must_use]
    pub fn schema(&self) -> u32 {
        self.schema
    }

    /// The registry vocabulary this catalogue was audited against. Must equal
    /// [`Registry::vocabulary_version`](crate::Registry::vocabulary_version) —
    /// the two vendored files move together or not at all.
    #[must_use]
    pub fn vocabulary(&self) -> Option<u32> {
        self.vocabulary
    }

    /// The content digest of the bytes this catalogue was built from.
    #[must_use]
    pub fn digest(&self) -> &str {
        &self.digest
    }

    /// What upstream's effects cache identity records this catalogue as
    /// (`catalog.rb:158`): the schema AND the content digest, so a re-audited
    /// row invalidates persisted summaries the same way a shape change does.
    #[must_use]
    pub fn identity(&self) -> String {
        format!("{}:{}", self.schema, self.digest)
    }

    /// Every catalogued class name, sorted — the surface a data spec walks.
    #[must_use]
    pub fn class_names(&self) -> Vec<&str> {
        self.classes.keys().map(String::as_str).collect()
    }

    /// The resolved class, or `None` for a class the catalogue does not list.
    #[must_use]
    pub fn class_entry(&self, owner: &str) -> Option<&ClassEntry> {
        self.classes.get(owner)
    }

    /// The `defaults:` postures, by name.
    #[must_use]
    pub fn posture_labels(&self, name: &str) -> Option<&[String]> {
        self.postures.get(name).map(Vec::as_slice)
    }

    /// The `universal:` selectors — present on every receiver, touching
    /// nothing.
    #[must_use]
    pub fn universal(&self) -> &BTreeSet<String> {
        &self.universal
    }

    /// Whether `name` is a constant that names an OBJECT rather than a class.
    #[must_use]
    pub fn object_constant(&self, name: &str) -> bool {
        self.object_constants.contains(name)
    }

    /// What `owner#selector` contributes on the instance side, consulting the
    /// posture — upstream's `lookup(owner, name)` defaults.
    #[must_use]
    pub fn lookup(&self, owner: &str, selector: &str) -> Option<&Entry> {
        self.lookup_with(owner, selector, false, true)
    }

    /// What `owner#selector` (or `owner.selector`, with `singleton`)
    /// contributes, or `None` when the catalogue has nothing to say — which is
    /// NOT a taint, and leaves the caller to treat the call as ordinary.
    ///
    /// **The precedence is the point** (`catalog.rb:184`): the class's own row
    /// first, then the 34-name `universal:` list, then the class's posture. A
    /// class's own row therefore wins over the universal ∅, and the universal ∅
    /// wins over a world-facing posture — without which the posture would put a
    /// wrong label on the most-called methods in Ruby, and a wrong label is
    /// worse than a missing one.
    ///
    /// `posture = false` asks for a ROW ONLY, which is how upstream's collector
    /// suppresses the class default where it would be wrong: an implicit-self
    /// call spells `Kernel#name`, and defaulting every unqualified call in a
    /// project body to `io` would colour the world.
    ///
    /// Carve-out: a narrowed row answers its UNNARROWED entry, which is exactly
    /// upstream's answer when it is handed no call node. A caller that HAS the
    /// call node must go through [`Self::resolve`] instead.
    #[must_use]
    pub fn lookup_with(
        &self,
        owner: &str,
        selector: &str,
        singleton: bool,
        posture: bool,
    ) -> Option<&Entry> {
        Some(match self.resolve(owner, selector, singleton, posture)? {
            Resolution::Row(row) => &row.entry,
            Resolution::Universal(entry) | Resolution::Posture(entry) => entry,
        })
    }

    /// The same precedence, resolved WITHOUT collapsing a narrowed row to its
    /// unnarrowed entry — which is the only form a slice-2 consumer may read.
    ///
    /// [`Self::lookup_with`] is upstream's `lookup(…, call_node: nil)`: sound,
    /// and an OVER the moment a caller with the call node in hand uses it,
    /// because the proven lane is graded as a raw string-set subset
    /// (ADR-0043 § 2 — `io.fs` where the oracle proved `io.fs.read` is an
    /// over-claim). `resolve` hands the [`Row`] back so the caller can branch on
    /// [`Row::narrow`] and run the handler over its own AST; the two ∅-shaped
    /// arms are returned distinctly for the same reason [`Entry::posture`]
    /// exists.
    #[must_use]
    pub fn resolve(
        &self,
        owner: &str,
        selector: &str,
        singleton: bool,
        posture: bool,
    ) -> Option<Resolution<'_>> {
        let entry = self.classes.get(owner)?;
        let bucket =
            if singleton { &entry.singleton_methods } else { &entry.instance_methods };
        if let Some(row) = bucket.get(selector) {
            return Some(Resolution::Row(row));
        }
        if self.universal.contains(selector) {
            return Some(Resolution::Universal(&UNIVERSAL_ENTRY));
        }
        if !posture {
            return None;
        }
        Some(Resolution::Posture(if singleton {
            &entry.singleton_posture_entry
        } else if entry.in_mutator_set(selector) {
            &entry.mutating_posture_entry
        } else {
            &entry.posture_entry
        }))
    }
}

/// Which tier of the precedence answered — the shape [`Catalog::resolve`]
/// returns, and the reason it exists rather than a bare [`Entry`].
#[derive(Debug, Clone, Copy)]
pub enum Resolution<'a> {
    /// The class's own row. **May carry a `narrow:` handler** the caller has to
    /// apply; [`Row::labels`] is only the unnarrowed upper bound.
    Row(&'a Row),
    /// The 34-name `universal:` list — an `Object`-level selector that exists on
    /// every receiver and touches nothing.
    Universal(&'a Entry),
    /// The class's default posture. Never carries a handler.
    Posture(&'a Entry),
}

impl RawClass {
    fn empty() -> Self {
        Self {
            posture: None,
            singleton_posture: None,
            kind: None,
            mutators: None,
            why: None,
            methods: None,
            singleton_methods: None,
        }
    }
}

/// `LabelSet.new(...)`'s normalisation: de-duplicated, sorted, and every member
/// checked against the grammar (upstream raises `ArgumentError` otherwise).
fn label_set(context: &str, labels: Option<Vec<String>>) -> Result<Vec<String>, Error> {
    let labels = labels.unwrap_or_default();
    for candidate in &labels {
        if !label::valid(candidate) {
            return Err(Error::InvalidLabel {
                context: context.to_string(),
                label: candidate.clone(),
            });
        }
    }
    Ok(labels.into_iter().collect::<BTreeSet<_>>().into_iter().collect())
}

fn build_class(
    name: &str,
    body: RawClass,
    postures: &BTreeMap<String, Vec<String>>,
) -> Result<ClassEntry, Error> {
    let mutators = match &body.mutators {
        None => None,
        Some(set) if MUTATOR_SETS.contains(&set.as_str()) => Some(set.clone()),
        Some(set) => {
            return Err(Error::UnknownMutatorSet { class: name.to_string(), set: set.clone() });
        }
    };
    // Upstream's `resolve_mutators` (`catalog.rb:238`) — the NAME expanded
    // through the reference's own three literals. A class with no `mutators:`
    // key gets the empty set, so `in_mutator_set` is a plain lookup everywhere.
    let mutator_selectors = crate::mutators().set(mutators.as_deref()).clone();
    let posture_labels = resolve_posture(name, body.posture.as_deref(), postures)?;
    let singleton_posture_labels = resolve_posture(
        name,
        body.singleton_posture.as_deref().or(body.posture.as_deref()),
        postures,
    )?;
    // Only the INSTANCE bucket is handed the mutator set, exactly as upstream
    // does (`catalog.rb:222-223`): `Array.push` is not a receiver mutation, and
    // a singleton row that happens to share a mutator's name must not read as
    // one.
    let instance_methods = build_rows(name, "#", body.methods, &mutator_selectors)?;
    let singleton_methods = build_rows(name, ".", body.singleton_methods, &BTreeSet::new())?;

    Ok(ClassEntry {
        instance_methods,
        singleton_methods,
        posture: body.posture,
        // Memoised rather than built per call — the lookup sits inside the
        // effect scan's walk.
        posture_entry: Entry {
            labels: posture_labels.clone(),
            mutates_receiver: false,
            from_posture: true,
        },
        mutating_posture_entry: Entry {
            labels: posture_labels.clone(),
            mutates_receiver: true,
            from_posture: true,
        },
        singleton_posture_entry: Entry {
            labels: singleton_posture_labels.clone(),
            mutates_receiver: false,
            from_posture: true,
        },
        posture_labels,
        singleton_posture_labels,
        mutators,
        mutator_selectors,
        object: body.kind.as_deref() == Some("object"),
        why: body.why.unwrap_or_default(),
    })
}

fn resolve_posture(
    class_name: &str,
    posture: Option<&str>,
    postures: &BTreeMap<String, Vec<String>>,
) -> Result<Vec<String>, Error> {
    let Some(posture) = posture else { return Ok(Vec::new()) };
    postures.get(posture).cloned().ok_or_else(|| Error::UnknownPosture {
        class: class_name.to_string(),
        posture: posture.to_string(),
    })
}

fn build_rows(
    class_name: &str,
    joiner: &str,
    rows: Option<BTreeMap<String, Option<RawRow>>>,
    mutators: &BTreeSet<String>,
) -> Result<BTreeMap<String, Row>, Error> {
    let mut built = BTreeMap::new();
    for (selector, body) in rows.unwrap_or_default() {
        let key = format!("{class_name}{joiner}{selector}");
        let in_mutator_set = mutators.contains(&selector);
        let row = build_row(&key, body.unwrap_or(RawRow::empty()), in_mutator_set)?;
        built.insert(selector, row);
    }
    Ok(built)
}

impl RawRow {
    fn empty() -> Self {
        Self { effects: None, narrow: None, mutates: None, why: None }
    }
}

fn build_row(key: &str, body: RawRow, in_mutator_set: bool) -> Result<Row, Error> {
    if let Some(handler) = &body.narrow {
        if !NARROWING_HANDLERS.contains(&handler.as_str()) {
            return Err(Error::UnknownNarrowingHandler {
                key: key.to_string(),
                handler: handler.clone(),
            });
        }
    }
    if body.why.as_deref().unwrap_or_default().is_empty() {
        return Err(Error::MissingWhy { key: key.to_string() });
    }
    let labels = label_set(key, body.effects)?;
    // Upstream's `mutates_receiver = body["mutates"] == "receiver" ||
    // in_mutator_set` (`catalog.rb:259`): an instance row whose selector is in
    // its class's set is a receiver mutation even though the row never says so
    // — `Array#push` has an `effects: []` row and mutates.
    let mutates_receiver = body.mutates.as_deref() == Some("receiver") || in_mutator_set;
    Ok(Row {
        narrow: body.narrow,
        why: body.why.unwrap_or_default(),
        entry: Entry { labels, mutates_receiver, from_posture: false },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog;

    // -----------------------------------------------------------------------
    // The lookup PRECEDENCE — row → universal → posture. Each arm is measured
    // against the pinned Ruby loader in the slice-1 probe § 1c.
    // -----------------------------------------------------------------------

    #[test]
    fn precedence_1_a_class_own_row_wins() {
        // `freeze` and `dup` are on the universal list AND rowed on Kernel;
        // `print` is rowed only. The row is consulted FIRST, so a world-facing
        // row is never swallowed by the universal ∅.
        let entry = catalog().lookup("Kernel", "print").expect("rowed");
        assert_eq!(entry.labels(), ["io.output.stdout"]);
        assert!(!entry.posture());
    }

    #[test]
    fn precedence_2_the_universal_list_beats_the_posture() {
        // Without this arm a world-facing posture would colour `socket.class`,
        // `io.respond_to?` and `handle.frozen?` — the most-called methods in
        // Ruby.
        for selector in ["class", "respond_to?", "frozen?", "inspect", "to_s", "is_a?", "tap"] {
            for owner in ["IO", "Socket"] {
                let entry = catalog().lookup(owner, selector).expect("universal");
                assert!(entry.labels().is_empty(), "{owner}#{selector} read as a world call");
                assert!(!entry.posture(), "{owner}#{selector} came from the posture");
            }
        }
    }

    #[test]
    fn precedence_3_the_posture_answers_last() {
        let entry = catalog().lookup("IO", "some_uncatalogued").expect("posture");
        assert_eq!(entry.labels(), ["io"]);
        assert!(entry.posture());

        let value = catalog().lookup("String", "some_uncatalogued").expect("posture");
        assert!(value.labels().is_empty());
        assert!(value.posture());
    }

    #[test]
    fn an_unlisted_class_contributes_nothing_at_all() {
        assert!(catalog().lookup("Tracer::Reporter", "report").is_none());
        assert!(catalog().lookup("Foo::Bar", "baz").is_none());
    }

    #[test]
    fn suppressing_the_posture_asks_for_a_row_only() {
        assert!(catalog().lookup_with("IO", "some_uncatalogued", false, false).is_none());
        assert!(catalog().lookup_with("Kernel", "puts", false, false).is_some());
        // The universal list is not a posture, so it still answers.
        assert!(catalog().lookup_with("IO", "class", false, false).is_some());
    }

    #[test]
    fn the_singleton_side_is_a_separate_bucket() {
        assert_eq!(catalog().lookup_with("File", "write", true, true).expect("rowed").labels(), [
            "io.fs.write"
        ]);
        // Kernel's singleton side is the `module_function` copy — a value
        // class, not the instance side's `world`.
        assert!(
            catalog()
                .lookup_with("Kernel", "Float", true, true)
                .expect("posture")
                .labels()
                .is_empty()
        );
        assert_eq!(catalog().lookup("Kernel", "puts").expect("rowed").labels(), [
            "io.output.stdout"
        ]);
    }

    #[test]
    fn from_posture_provenance_is_carried_not_inferred_from_emptiness() {
        // The distinction ADR-0043 slice 4 propagates: a posture answer keeps
        // the project edge because a project may reopen a core class. Both of
        // these are ∅; only one is a posture. `Thread.new`'s explicit ∅ row is
        // what stops it reading as an unresolved call while its block joins the
        // enclosing method by containment.
        let explicit_empty_row =
            catalog().lookup_with("Thread", "new", true, true).expect("rowed");
        assert!(explicit_empty_row.labels().is_empty());
        assert!(!explicit_empty_row.posture());

        let empty_posture = catalog().lookup("String", "some_uncatalogued").expect("posture");
        assert!(empty_posture.labels().is_empty());
        assert!(empty_posture.posture());
    }

    #[test]
    fn a_posture_answer_is_produced_for_a_class_with_no_row_at_all() {
        // `TCPSocket.new` has no row; the `net` posture answers, and slice 2's
        // origin key for it is still `catalogue:TCPSocket.new`.
        let entry = catalog().lookup_with("TCPSocket", "new", true, true).expect("posture");
        assert_eq!(entry.labels(), ["io.net"]);
        assert!(entry.posture());
    }

    // -----------------------------------------------------------------------
    // The carve-outs: one closed by slice 2, one still standing.
    // -----------------------------------------------------------------------

    #[test]
    fn a_narrowed_row_answers_its_unnarrowed_upper_bound() {
        // Upstream's own answer when handed no call node — the sound bound the
        // handler degrades to. `File.open` narrows to `io.fs.read` /
        // `io.fs.write` on a literal mode; unnarrowed it is `io.fs`.
        let entry = catalog().lookup_with("File", "open", true, true).expect("rowed");
        assert_eq!(entry.labels(), ["io.fs"]);

        let row = catalog()
            .class_entry("File")
            .expect("listed")
            .singleton_methods()
            .get("open")
            .expect("rowed");
        assert_eq!(row.narrow(), Some("file_open"));
    }

    #[test]
    fn every_narrowed_row_is_reachable_as_a_row_through_resolve() {
        // The probe's § 7c gate: a slice-2 consumer resolves, sees `Row`, and
        // branches on `narrow()`. A row that answered through the UNIVERSAL or
        // POSTURE arm instead would silently hand the consumer the unnarrowed
        // upper bound and turn `File.open(p, "w")` into an OVER.
        let narrowed = [
            ("File", "open", true),
            ("Kernel", "open", false),
            ("Pathname", "open", false),
            ("Time", "new", true),
            ("Random", "new", true),
            ("URI", "open", true),
            ("OpenURI", "open_uri", true),
        ];
        for (owner, selector, singleton) in narrowed {
            let resolved = catalog()
                .resolve(owner, selector, singleton, true)
                .unwrap_or_else(|| panic!("{owner}.{selector} answers nothing"));
            let Resolution::Row(row) = resolved else {
                panic!("{owner}.{selector} did not answer as a ROW: {resolved:?}");
            };
            assert!(row.narrow().is_some(), "{owner}.{selector} lost its narrow: handler");
        }
        // …and exactly seven rows in the shipped catalogue carry one, so a
        // handler upstream ADDS fails here rather than in a corpus nobody runs.
        let with_handler = catalog()
            .class_names()
            .iter()
            .flat_map(|name| {
                let entry = catalog().class_entry(name).expect("listed");
                entry.instance_methods().values().chain(entry.singleton_methods().values())
            })
            .filter(|row| row.narrow().is_some())
            .count();
        assert_eq!(with_handler, narrowed.len());
    }

    #[test]
    fn the_mutator_set_is_expanded_on_the_row_and_the_posture_path() {
        // SLICE-2: the slice-1 carve-out, closed. Upstream answers
        // `mutates_receiver == true` for `Array#push` by expanding
        // `MutationWidening::ARRAY_MUTATORS`; the port now expands the same
        // three literals, extracted into `vendor/effects/mutators.yml`.
        //
        // The three value classes row almost nothing, so on the SHIPPED
        // catalogue every mutator answers through the POSTURE arm — the one
        // slice 1's pin named. `Array#push` and `String#squeeze!` both read
        // `false` before this slice.
        for (owner, selector) in [("Array", "push"), ("Hash", "store"), ("String", "squeeze!")] {
            let posture = catalog().lookup(owner, selector).expect("posture");
            assert!(posture.posture(), "{owner}#{selector} is not the posture arm");
            assert!(posture.mutates_receiver(), "{owner}#{selector} is in its mutator set");
            assert!(posture.labels().is_empty(), "…and still a `value` posture");
        }
        // …and a non-mutator on the same class stays non-mutating, which is what
        // makes this an expansion rather than a class-wide flag.
        let read_only = catalog().lookup("Array", "map").expect("posture");
        assert!(!read_only.mutates_receiver());

        assert_eq!(catalog().class_entry("Array").expect("listed").mutator_set(), Some("array"));
        assert!(catalog().class_entry("Array").expect("listed").in_mutator_set("push"));

        // An EXPLICIT `mutates: receiver` row still reads.
        assert!(catalog().lookup("Array", "shuffle!").expect("rowed").mutates_receiver());

        // Three classes name a set; nothing else expands one.
        let with_set: Vec<&str> = catalog()
            .class_names()
            .into_iter()
            .filter(|name| catalog().class_entry(name).expect("listed").mutator_set().is_some())
            .collect();
        assert_eq!(with_set, ["Array", "Hash", "String"]);
    }

    #[test]
    fn an_in_set_row_mutates_even_though_the_row_never_says_so() {
        // The ROW disjunct of upstream's `mutates_receiver = body["mutates"] ==
        // "receiver" || in_mutator_set`. It is unreachable on the SHIPPED
        // catalogue — `Array#shuffle!` is the only rowed selector that is also
        // in its class's set, and it declares `mutates: receiver` outright — so
        // it is pinned over synthetic bytes instead of going untested.
        let source = concat!(
            "schema: 1\ndefaults:\n  value:\nclasses:\n  Array:\n    posture: value\n",
            "    mutators: array\n",
            "    methods:\n      push: { effects: [], why: rowed but silent }\n",
            "      map: { effects: [], why: rowed and pure }\n"
        );
        let catalog = Catalog::from_yaml_str(source).expect("parses");
        let pushed = catalog.lookup("Array", "push").expect("rowed");
        assert!(!pushed.posture(), "the row must win over the posture");
        assert!(pushed.mutates_receiver(), "an in-set ROW is a receiver mutation");
        assert!(!catalog.lookup("Array", "map").expect("rowed").mutates_receiver());
    }

    #[test]
    fn only_the_instance_side_expands_the_mutator_set() {
        // `Array.new` is a singleton row and `new` is not a mutator anywhere;
        // the load-bearing half is that the singleton BUCKET is built with no
        // set at all, so a singleton selector sharing a mutator's name (`Hash`'s
        // `[]`-family, `String.replace`-shaped) can never read as a receiver
        // mutation. Upstream passes `nil` there (`catalog.rb:223`).
        let entry = catalog().class_entry("String").expect("listed");
        assert!(entry.in_mutator_set("replace"));
        for row in entry.singleton_methods().values() {
            assert!(!row.mutates_receiver(), "a singleton row expanded the mutator set");
        }
        // The singleton POSTURE entry likewise: `String.some_mutator_named_thing`
        // is a call on the class object.
        let singleton_posture =
            catalog().lookup_with("String", "squeeze!", true, true).expect("posture");
        assert!(singleton_posture.posture());
        assert!(!singleton_posture.mutates_receiver());
    }

    // -----------------------------------------------------------------------
    // Shape, and the object-constant surface.
    // -----------------------------------------------------------------------

    #[test]
    fn the_shipped_catalogue_has_the_shape_the_probe_measured() {
        let catalog = catalog();
        assert_eq!(catalog.schema(), 1);
        assert_eq!(catalog.class_names().len(), 80);
        assert_eq!(catalog.universal().len(), 34);
        assert_eq!(
            ["value", "world", "fs", "net", "ipc", "http", "process", "signal", "global",
             "nondet", "ffi", "stdout", "stderr", "stdin"]
                .iter()
                .filter(|name| catalog.posture_labels(name).is_some())
                .count(),
            14
        );

        let (instance, singleton) = catalog.class_names().iter().fold((0, 0), |acc, name| {
            let entry = catalog.class_entry(name).expect("listed");
            (acc.0 + entry.instance_methods().len(), acc.1 + entry.singleton_methods().len())
        });
        assert_eq!((instance, singleton, instance + singleton), (216, 204, 420));
    }

    #[test]
    fn an_explicit_empty_effects_list_is_not_the_same_as_no_row() {
        let empty_rows = catalog()
            .class_names()
            .iter()
            .flat_map(|name| {
                let entry = catalog().class_entry(name).expect("listed");
                entry
                    .instance_methods()
                    .values()
                    .chain(entry.singleton_methods().values())
            })
            .filter(|row| row.labels().is_empty())
            .count();
        assert_eq!(empty_rows, 77, "the ∅-row count the probe measured");
    }

    #[test]
    fn the_five_object_constants_are_marked() {
        for name in ["ENV", "ARGF", "STDIN", "STDOUT", "STDERR"] {
            assert!(catalog().object_constant(name), "{name} must be kind: object");
        }
        assert!(!catalog().object_constant("File"));
        assert!(!catalog().object_constant("IO"));
        assert_eq!(
            catalog()
                .class_names()
                .iter()
                .filter(|name| catalog().object_constant(name))
                .count(),
            5
        );
    }

    #[test]
    fn kernel_is_the_only_class_with_its_own_singleton_posture() {
        let with_own: Vec<&str> = catalog()
            .class_names()
            .into_iter()
            .filter(|name| {
                let entry = catalog().class_entry(name).expect("listed");
                entry.posture_labels() != entry.singleton_posture_labels()
            })
            .collect();
        assert_eq!(with_own, ["Kernel"]);
    }

    // -----------------------------------------------------------------------
    // Load-time refusals — upstream's, ported case for case.
    // -----------------------------------------------------------------------

    #[test]
    fn a_row_with_no_justification_is_refused() {
        let source = "schema: 1\nclasses:\n  Foo:\n    methods:\n      bar: {}\n";
        let error = Catalog::from_yaml_str(source).expect_err("must refuse");
        assert!(matches!(error, Error::MissingWhy { .. }), "{error}");
        assert!(error.to_string().contains("why:"), "{error}");
    }

    #[test]
    fn a_posture_no_default_declares_is_refused() {
        let source = "schema: 1\nclasses:\n  Foo:\n    posture: nope\n";
        let error = Catalog::from_yaml_str(source).expect_err("must refuse");
        assert!(matches!(error, Error::UnknownPosture { .. }), "{error}");
        assert!(error.to_string().contains("unknown posture"), "{error}");
    }

    #[test]
    fn a_narrowing_handler_that_does_not_exist_is_refused() {
        let source = concat!(
            "schema: 1\nclasses:\n  Foo:\n    methods:\n",
            "      bar: { narrow: nope, why: x }\n"
        );
        let error = Catalog::from_yaml_str(source).expect_err("must refuse");
        assert!(matches!(error, Error::UnknownNarrowingHandler { .. }), "{error}");
        assert!(error.to_string().contains("narrowing handler"), "{error}");
    }

    #[test]
    fn a_mutator_set_that_does_not_exist_is_refused() {
        let source = "schema: 1\nclasses:\n  Foo:\n    mutators: nope\n";
        let error = Catalog::from_yaml_str(source).expect_err("must refuse");
        assert!(matches!(error, Error::UnknownMutatorSet { .. }), "{error}");
    }

    #[test]
    fn a_label_outside_the_grammar_is_refused() {
        let source = concat!(
            "schema: 1\nclasses:\n  Foo:\n    methods:\n",
            "      bar: { effects: [IO.Net], why: x }\n"
        );
        let error = Catalog::from_yaml_str(source).expect_err("must refuse");
        assert!(matches!(error, Error::InvalidLabel { .. }), "{error}");
    }

    #[test]
    fn an_unknown_key_is_refused() {
        // Deliberately stricter than upstream — see the raw-shape comment.
        assert!(Catalog::from_yaml_str("schema: 1\nnovel: 3\n").is_err());
        assert!(
            Catalog::from_yaml_str("schema: 1\nclasses:\n  Foo:\n    novel: 3\n").is_err()
        );
    }

    #[test]
    fn the_merge_key_selector_survives_the_parse() {
        // `<<` is spelt `!!str "<<"` in the data file: a bare (even quoted)
        // `<<` key is YAML's MERGE key, and a loader that resolves it would
        // splice the row's mapping into the enclosing `methods:` map instead of
        // filing it under the selector. Four rows depend on this.
        assert_eq!(catalog().lookup("IO", "<<").expect("rowed").labels(), ["io"]);
        assert_eq!(catalog().lookup("File", "<<").expect("rowed").labels(), ["io.fs.write"]);
        assert_eq!(catalog().lookup("Logger", "<<").expect("rowed").labels(), [
            "io", "telemetry"
        ]);
        assert_eq!(catalog().lookup("SizedQueue", "<<").expect("rowed").labels(), ["io"]);
        // …and exactly four rows carry it, so a loader that swallowed one
        // silently would fail here rather than in a corpus nobody runs.
        let merge_rows = catalog()
            .class_names()
            .iter()
            .filter(|name| {
                let entry = catalog().class_entry(name).expect("listed");
                entry.instance_methods().contains_key("<<")
                    || entry.singleton_methods().contains_key("<<")
            })
            .count();
        assert_eq!(merge_rows, 4);
    }
}
