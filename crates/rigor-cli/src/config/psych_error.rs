//! The fields of the `Psych::SyntaxError` the reference rescues in
//! `Configuration.read_yaml` (configuration.rb:471-476), read straight off
//! libyaml's parser.
//!
//! Psych's `make_exception` (psych_parser.c) builds the error from the parser
//! struct: `line`/`column` are ALWAYS `context_mark + 1`, whether or not the
//! error has a context, and `problem`/`context` are the raw C strings.
//! serde_yaml's `Error` Display hides a zero mark and the context mark when it
//! equals the problem mark, so the position cannot be recovered from that
//! text: `...` prints no position at all (Psych `1:1`), and `[a` shows only
//! the problem mark (`2:1`; Psych reports the `[` at `1:1`). So when
//! serde_yaml rejects a file, this re-runs the same libyaml (unsafe-libyaml,
//! the crate serde_yaml drives, a line-for-line port of libyaml 0.2.5) over
//! the same text and reads the error it stops on.

use regex::Regex;
use std::ffi::CStr;
use std::mem::MaybeUninit;
use std::sync::OnceLock;

/// `Psych::SyntaxError`'s `line`, `column`, `problem`, `context`.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct SyntaxError {
    pub line: usize,
    pub column: usize,
    pub problem: String,
    pub context: Option<String>,
}

/// The libyaml error that parsing `text` stops on, or `None` when the first
/// document parses. Only the first document is read: `Psych.parse` returns
/// from `parse_stream` at the first document's end, and serde_yaml's
/// `Deserializer::next` loads only that document too. The encoding and input
/// are set exactly as serde_yaml's parser sets them, so both runs see the
/// same event stream.
pub(super) fn first_document_error(text: &str) -> Option<SyntaxError> {
    // Boxed: `yaml_parser_set_input_string` stores a pointer to the parser in
    // the parser itself (`read_handler_data`), so it must not move.
    let mut parser = Box::new(MaybeUninit::<unsafe_libyaml::yaml_parser_t>::uninit());
    // SAFETY: mirrors serde_yaml's `libyaml::parser::Parser::new`/`next`: the
    // parser is initialized before use and deleted exactly once, `text`
    // outlives it, each event is deleted after it is read, and the error
    // strings (static C literals owned by libyaml) are copied out before the
    // delete.
    unsafe {
        let parser = parser.as_mut_ptr();
        if unsafe_libyaml::yaml_parser_initialize(parser).fail {
            return None;
        }
        unsafe_libyaml::yaml_parser_set_encoding(parser, unsafe_libyaml::YAML_UTF8_ENCODING);
        unsafe_libyaml::yaml_parser_set_input_string(parser, text.as_ptr(), text.len() as u64);
        let mut out = None;
        loop {
            let mut event = MaybeUninit::<unsafe_libyaml::yaml_event_t>::uninit();
            if unsafe_libyaml::yaml_parser_parse(parser, event.as_mut_ptr()).fail {
                // The error fields are public only through `Deref`, so read
                // them via a shared borrow (no `&mut` to the parser is live).
                let p = &*parser;
                if p.error != unsafe_libyaml::YAML_MEMORY_ERROR {
                    out = Some(SyntaxError {
                        line: p.context_mark.line as usize + 1,
                        column: p.context_mark.column as usize + 1,
                        problem: c_string(p.problem).unwrap_or_default(),
                        context: c_string(p.context),
                    });
                }
                break;
            }
            let kind = (*event.as_ptr()).type_;
            unsafe_libyaml::yaml_event_delete(event.as_mut_ptr());
            if kind == unsafe_libyaml::YAML_DOCUMENT_END_EVENT
                || kind == unsafe_libyaml::YAML_STREAM_END_EVENT
            {
                break;
            }
        }
        unsafe_libyaml::yaml_parser_delete(parser);
        out
    }
}

/// What `YAML.safe_load_file(absolute, aliases: false)` dies on while
/// VISITING the first document — a tier distinct from the `Psych::SyntaxError`
/// [`first_document_error`] renders, because `Psych.safe_load` runs `parse` to
/// completion before `NoAliasRuby` / the restricted `ScalarScanner` see a node
/// (probe: `x: *a\ny: [unclosed` reports the syntax error, not the alias).
#[derive(Debug, PartialEq, Eq)]
pub(super) enum SafeLoadRejection {
    /// `Psych::AliasesNotEnabled` — `NoAliasRuby#visit_Psych_Nodes_Alias`
    /// raises on ANY alias node, anchored (`*a`) or not (`*nope`).
    Alias,
    /// `Psych::DisallowedClass` ("Tried to load unspecified class: X") — a
    /// scalar `ScalarScanner#tokenize` / `ToRuby#deserialize` resolves through
    /// the restricted `ClassLoader`, whose `permitted_classes` is `[]`.
    DisallowedClass(String),
}

/// The first node — in the visitor's document order — that
/// `YAML.safe_load_file`'s post-parse walk raises on, or `None` when the first
/// document parses without one. A parse failure yields `None` on purpose: the
/// `Psych::SyntaxError` tier fires first upstream (the document is parsed
/// whole, then visited), so the caller falls back to the `not valid YAML`
/// render. Only the first document is read, like [`first_document_error`]
/// (probe: an alias in a second document never fires).
///
/// # Safety note
/// Same libyaml contract as [`first_document_error`].
pub(super) fn safe_load_rejection(text: &str) -> Option<SafeLoadRejection> {
    let mut parser = Box::new(MaybeUninit::<unsafe_libyaml::yaml_parser_t>::uninit());
    unsafe {
        let parser = parser.as_mut_ptr();
        if unsafe_libyaml::yaml_parser_initialize(parser).fail {
            return None;
        }
        unsafe_libyaml::yaml_parser_set_encoding(parser, unsafe_libyaml::YAML_UTF8_ENCODING);
        unsafe_libyaml::yaml_parser_set_input_string(parser, text.as_ptr(), text.len() as u64);

        let mut out = None;
        let mut parsed = true;
        // A collection tag whose visitor semantics the scan does not model
        // stops RECORDING (`scan = false`) but the event drain must continue
        // to the document's end: `Psych.safe_load` parses the document
        // whole, THEN walks it, so a `Psych::SyntaxError` anywhere later in
        // the document still outranks a death recorded before the tag.
        let mut scan = true;
        // `!str`-family MAPPINGs need post-order tracking (see
        // [`StrMapTracker`]): the arm visits every child, then crashes in
        // `init_with` when a non-`str` member exists and the `str` value
        // is missing or frozen — siblings after the mapping's END are
        // never visited upstream.
        let mut str_maps: Vec<StrMapTracker> = Vec::new();
        let mut collection_depth: u32 = 0;
        loop {
            let mut event = MaybeUninit::<unsafe_libyaml::yaml_event_t>::uninit();
            if unsafe_libyaml::yaml_parser_parse(parser, event.as_mut_ptr()).fail {
                parsed = false;
                break;
            }
            let kind = (*event.as_ptr()).type_;
            match kind {
                unsafe_libyaml::YAML_ALIAS_EVENT if scan && out.is_none() => {
                    out = Some(SafeLoadRejection::Alias);
                }
                unsafe_libyaml::YAML_SCALAR_EVENT if scan && out.is_none() => {
                    let scalar = &(*event.as_ptr()).data.scalar;
                    let value = if scalar.value.is_null() {
                        String::new()
                    } else {
                        String::from_utf8_lossy(std::slice::from_raw_parts(
                            scalar.value,
                            scalar.length as usize,
                        ))
                        .into_owned()
                    };
                    let tag = c_string(scalar.tag.cast());
                    if let Some(tracker) = str_maps.last_mut() {
                        tracker.scalar_slot(collection_depth, &value, scalar.quoted_implicit);
                    }
                    match scalar_rejection(tag.as_deref(), &value, scalar.quoted_implicit) {
                        ScalarOutcome::Permitted => {}
                        ScalarOutcome::Death(rejection) => out = Some(rejection),
                        // A death the scan does not model — `Float()`
                        // `ArgumentError`, `Encoding.find` — ends the walk
                        // HERE upstream: stop recording so a later
                        // sibling is never named.
                        ScalarOutcome::Unmodelled => scan = false,
                    }
                }
                unsafe_libyaml::YAML_MAPPING_START_EVENT
                | unsafe_libyaml::YAML_SEQUENCE_START_EVENT => {
                    let mapping = kind == unsafe_libyaml::YAML_MAPPING_START_EVENT;
                    let tag_ptr = if mapping {
                        (*event.as_ptr()).data.mapping_start.tag
                    } else {
                        (*event.as_ptr()).data.sequence_start.tag
                    };
                    let tag = c_string(tag_ptr.cast());
                    if let Some(tracker) = str_maps.last_mut() {
                        tracker.collection_slot(collection_depth);
                    }
                    collection_depth += 1;
                    if tag.as_deref().is_some_and(|t| collection_tag_bails(t, mapping)) {
                        // A shape the scan declines: serde_yaml's own pass
                        // decides this document (a `!ruby/*` tag dies there
                        // at exit 64), or the visitor walks the node through
                        // an arm outside `DisallowedClass` (`!omap` seq's
                        // first/last member pairs, empty-name
                        // `nil.allocate`). Either way the scan stops
                        // recording rather than mis-order a death or name a
                        // class the visitor never reached.
                        scan = false;
                    } else if scan && out.is_none() {
                        out = collection_tag_death(tag.as_deref(), mapping);
                        if out.is_none() && mapping && is_str_mapping(tag.as_deref()) {
                            str_maps.push(StrMapTracker::new(collection_depth));
                        }
                    }
                }
                unsafe_libyaml::YAML_MAPPING_END_EVENT
                | unsafe_libyaml::YAML_SEQUENCE_END_EVENT => {
                    if str_maps
                        .last()
                        .is_some_and(|tracker| tracker.depth == collection_depth)
                    {
                        // `init_with` runs after every child has visited:
                        // non-`str` members plus a missing-or-frozen `str`
                        // value raise the unmodelled `FrozenError` there,
                        // so a sibling after this mapping is never visited.
                        if str_maps
                            .last()
                            .is_some_and(|tracker| tracker.freezes_at_init_with())
                        {
                            scan = false;
                        }
                        str_maps.pop();
                    }
                    collection_depth = collection_depth.saturating_sub(1);
                }
                _ => {}
            }
            unsafe_libyaml::yaml_event_delete(event.as_mut_ptr());
            if kind == unsafe_libyaml::YAML_DOCUMENT_END_EVENT
                || kind == unsafe_libyaml::YAML_STREAM_END_EVENT
            {
                break;
            }
        }
        unsafe_libyaml::yaml_parser_delete(parser);

        // A syntax error anywhere in the first document wins over every
        // visitor death: `Psych.safe_load` parses, THEN walks.
        if parsed { out } else { None }
    }
}

/// A collection tag whose upstream visitor arm the scan does NOT model —
/// the document's fate is left to serde_yaml's own pass (which itself
/// rejects `!ruby/*`) rather than guessed at:
///
/// - `!ruby/*` on a collection — `visit_Psych_Nodes_{Mapping,Sequence}`
///   resolves the tag through per-tag `resolve_class` arms plus partial
///   member walks (`hash-with-ivars` skips every key but `elements`/
///   `ivars`, so `other: 2024-01-01` LOADS upstream): #347's declared
///   out-of-scope.
/// - `!omap`/`!!omap` on a SEQUENCE — the `Psych::Omap.new` arm visits only
///   each element's FIRST/LAST child (`[1, 2024-01-01, 2]`'s middle member
///   is never seen upstream) and `NoMethodError`s on a scalar element.
/// - `!map:`/`!seq:` with an EMPTY name — `resolve_class('')` →
///   `nil.allocate` → `NoMethodError`, outside the `DisallowedClass` model.
fn collection_tag_bails(tag: &str, mapping: bool) -> bool {
    tag.starts_with("!ruby/")
        || (!mapping && (tag == "!omap" || tag == "tag:yaml.org,2002:omap"))
        || (mapping && tag == "!map:")
        || (!mapping && tag == "!seq:")
}

/// `Psych::DisallowedClass` for a COLLECTION's tag, if
/// `visit_Psych_Nodes_{Mapping,Sequence}` resolves it through the restricted
/// `ClassLoader` BEFORE its children — the non-`!ruby` slice of the `case
/// o.tag` arms (probe-verified: `!set`/`!!set` → `Psych::Set`,
/// `!omap`/`!!omap` → `Psych::Omap` on a MAPPING only, `!map:K`/`!seq:K`/
/// `!str:K` → `K`). `!omap` sequences and empty `!map:`/`!seq:` names are
/// [`collection_tag_bails`] shapes — the scan reaches here only past them.
fn collection_tag_death(tag: Option<&str>, mapping: bool) -> Option<SafeLoadRejection> {
    let disallowed = |name: &str| Some(SafeLoadRejection::DisallowedClass(name.to_string()));
    match tag? {
        t if mapping && (t == "!set" || t == "tag:yaml.org,2002:set") => {
            disallowed("Psych::Set")
        }
        t if mapping && (t == "!omap" || t == "tag:yaml.org,2002:omap") => {
            disallowed("Psych::Omap")
        }
        // An empty name reaches `resolve_class('')` → `nil.allocate`
        // (`NoMethodError`) upstream — a crash shape outside this scan's
        // `DisallowedClass` model, left to the document's own fate.
        t if mapping && t.starts_with("!map:") && t.len() > "!map:".len() => {
            disallowed(&t["!map:".len()..])
        }
        t if !mapping && t.starts_with("!seq:") && t.len() > "!seq:".len() => {
            disallowed(&t["!seq:".len()..])
        }
        t if mapping && t.starts_with("!str:") && t.len() > "!str:".len() => {
            disallowed(&t["!str:".len()..])
        }
        // Everything else VISITS every child upstream — an empty name on
        // the OPPOSITE kind (or an empty `!str:`) falls to the `else` arm
        // (probe: `x: !map: [2024-01-01]`, `x: !seq: {a: 2024-01-01}` and
        // `x: !str: {a: 2024-01-01}` all die `Date` on both engines), so a
        // child death the scan records INSIDE the collection is one the
        // visitor reaches. The one AFTER-arm exception — a `!str`-family
        // MAPPING, whose `init_with` runs past the children and can raise
        // `FrozenError` — is tracked by [`StrMapTracker`], not here.
        _ => None,
    }
}

/// What `safe_load`'s visitor does at one unquoted SCALAR.
enum ScalarOutcome {
    /// The scalar loads (or a later node decides the document).
    Permitted,
    /// The visitor dies a modeled death here.
    Death(SafeLoadRejection),
    /// The visitor dies an UNMODELED death here — `ArgumentError` from
    /// `Float()` or `Encoding.find`, a `TypeError` from `Float(true)` —
    /// a shape outside the `DisallowedClass` model, so the scan stops
    /// recording: upstream's walk ends at this node.
    Unmodelled,
}

/// One scalar event's fate in `safe_load`'s visitor: `ToRuby#deserialize`
/// returns `o.value` untouched only when the scalar is QUOTED — and
/// `Nodes::Scalar#quoted` is libyaml's `quoted_implicit`, which is FALSE
/// whenever a tag is written (probe: `!foo "2024-01-01"` is `quoted=false`
/// on `Psych.parse` and dies `Date` upstream). An untagged plain scalar
/// goes to `ScalarScanner#tokenize`; explicit tags take `deserialize`'s
/// `case o.tag` arms — three of which raise outside the model instead.
fn scalar_rejection(tag: Option<&str>, value: &str, quoted: bool) -> ScalarOutcome {
    if quoted {
        return ScalarOutcome::Permitted;
    }
    if let Some(class) = disallowed_class(tag, value) {
        return ScalarOutcome::Death(SafeLoadRejection::DisallowedClass(class));
    }
    match tag {
        // `::Encoding.find` loads the name or raises `ArgumentError` (probe:
        // `nope`) — either way outside the model, and every later sibling
        // goes unvisited upstream.
        Some("!ruby/encoding") => ScalarOutcome::Unmodelled,
        // `Float(@ss.tokenize(o.value))`: the tokenizer's own class deaths
        // were handled above; whatever survives them must still read as a
        // Ruby Float or the conversion raises (`!float abc` →
        // `ArgumentError`, `!float true` → `TypeError`). The port declines
        // a few forms the reference accepts through numeric tokenize
        // results (`1_000`, `1:20`, `.inf`) — the declining side.
        Some("!float") | Some("tag:yaml.org,2002:float") if !ruby_float_value(value) => {
            ScalarOutcome::Unmodelled
        }
        // Untagged: the tokenizer's FLOAT arm matches a dot-mandatory
        // shape and runs `Float(delete(',_') + the \.([Ee]|$) rewrite)`,
        // which raises exactly when no digit survives (probe: `.e+1` →
        // `e+1`, `-.E-5` → `-E-5`).
        None if float_token_crashes(value) => ScalarOutcome::Unmodelled,
        _ => ScalarOutcome::Permitted,
    }
}

/// Would Ruby's `Float(string)` accept `s`? YAML strips a plain scalar's
/// surrounding whitespace, so the bare grammar decides: an optional sign,
/// digits around an optional dot, and an optional `e`/`E` exponent —
/// never an empty mantissa (probe: `Float("1E5")` and `Float("1e5")` both
/// load, `Float("e+1")` and `Float("-E-5")` both raise).
fn ruby_float_value(s: &str) -> bool {
    float_grammar_match(s)
}

/// Does untagged `tokenize`'s FLOAT arm raise on `s`? The arm skips the
/// bare-dot escape (`[-+]?\.` returns the String) and otherwise runs
/// `Float` over the `,`/`_`-stripped value with every `.` before `E`/`e`
/// or the end rewritten away — raising when no digit remains.
fn float_token_crashes(s: &str) -> bool {
    if !psych_float_match(s) || bare_dot_match(s) {
        return false;
    }
    let mut normalized = String::with_capacity(s.len());
    for (i, c) in s.char_indices() {
        if c == '_' || c == ',' {
            continue;
        }
        let after = i + c.len_utf8();
        let rewritten_away = c == '.' && (s[after..].starts_with(['e', 'E']) || after == s.len());
        if !rewritten_away {
            normalized.push(c);
        }
    }
    !float_grammar_match(&normalized)
}

/// A MAPPING whose tag routes to `visit_Psych_Nodes_Mapping`'s `str` arm —
/// `/^!(?:str|ruby\/string)(?::(.*))?$/` with NO name (a nil/empty
/// `resolve_class` yields nil) or `tag:yaml.org,2002:str`. A NON-empty
/// `!str:K` name resolves through the restricted loader first and dies
/// `DisallowedClass: K` at the mapping's START — not this tracker's
/// business.
fn is_str_mapping(tag: Option<&str>) -> bool {
    matches!(
        tag,
        Some("!str" | "tag:yaml.org,2002:str" | "!ruby/string" | "!str:" | "!ruby/string:")
    )
}

/// One `!str`-family MAPPING being drained. The arm (to_ruby.rb
/// `visit_Psych_Nodes_Mapping`, the `/^!(?:str|ruby\/string)/` case)
/// visits every child pair — child deaths fire BEFORE `init_with`, which
/// is why they keep recording — then `init_with` runs
/// `instance_variable_set` for every NON-`str` member onto the `str`
/// value: `FrozenError` when such a member exists and the value is
/// missing (`nil`) or frozen (every tokenize result but a String —
/// probe: `!str {a: 1}` and `!str {str: 1, a: 2}` crash, `!str {}` and
/// `!str {str: foo, a: 2}` complete). A sibling AFTER the mapping is
/// never visited upstream when it crashes, so the scan stops recording
/// at its END.
struct StrMapTracker {
    /// The collection depth of this mapping's own member events.
    depth: u32,
    /// Child slots filled at `depth` — even slots are keys, odd values.
    slot: u32,
    /// Key slots (pairs) at `depth`.
    pairs: u32,
    /// The pending key was the scalar `str`.
    str_key: bool,
    /// Some key was `str` (the last such pair decides the value).
    str_present: bool,
    /// The last `str` value was unfrozen: a String, or a nested
    /// collection (`Hash`/`Array` take instance variables fine).
    str_ok: bool,
}

impl StrMapTracker {
    fn new(depth: u32) -> Self {
        Self {
            depth,
            slot: 0,
            pairs: 0,
            str_key: false,
            str_present: false,
            str_ok: false,
        }
    }

    fn scalar_slot(&mut self, depth: u32, value: &str, quoted: bool) {
        if depth != self.depth {
            return;
        }
        if self.slot % 2 == 0 {
            self.pairs += 1;
            // `accept(k) == 'str'` — the raw text decides, quoted or not.
            self.str_key = value == "str";
        } else if self.str_key {
            self.str_present = true;
            self.str_ok = quoted || !tokenize_freezes(value);
            self.str_key = false;
        }
        self.slot += 1;
    }

    fn collection_slot(&mut self, depth: u32) {
        if depth != self.depth {
            return;
        }
        if self.slot % 2 == 0 {
            self.pairs += 1;
            // A collection key is not the scalar `str`.
            self.str_key = false;
        } else if self.str_key {
            // A collection value is an unfrozen `Hash`/`Array`.
            self.str_present = true;
            self.str_ok = true;
            self.str_key = false;
        }
        self.slot += 1;
    }

    /// `init_with`'s `FrozenError` predicate: members beyond the `str`
    /// pair exist, and the `str` value is missing or frozen.
    fn freezes_at_init_with(&self) -> bool {
        let members = self.pairs - u32::from(self.str_present);
        members > 0 && !(self.str_present && self.str_ok)
    }
}

/// The class a `Psych::DisallowedClass` reports for a PLAIN-style scalar, if
/// `deserialize`'s `case o.tag` routes it through the restricted
/// `ClassLoader`. `None` is every scalar landing on a class `safe_load`
/// permits by construction (String/Integer/Float/bool/nil). The `else` arm —
/// including `!float` (`Float(tokenize(...))` tokenizes first) and every
/// non-`!ruby` unknown tag — tokenizes the value text.
fn disallowed_class(tag: Option<&str>, value: &str) -> Option<String> {
    let Some(tag) = tag else {
        return tokenize_disallowed(value);
    };
    match tag {
        // `deserialize` arms that never reach the restricted class loader:
        // `!binary` unpacks to a String, `str` yields `o.value`, and
        // `!ruby/encoding` calls `Encoding.find` — which loads the
        // encoding or raises an `ArgumentError`, an uncaught shape whose
        // LATER siblings are unvisited upstream, so `scalar_rejection`
        // routes it to `Unmodelled` (and serde_yaml's own `!ruby/*`
        // rejection renders the document's exit 64 either way).
        "tag:yaml.org,2002:binary" | "!binary" | "tag:yaml.org,2002:str" | "!str"
        | "!ruby/string" | "!ruby/encoding" => None,
        // `/^!(?:str|ruby\/string)(?::(.*))?$/` → `resolve_class($1)` — the
        // restricted loader rejects any non-empty name.
        _ if tag.starts_with("!str:") => {
            let name = &tag["!str:".len()..];
            (!name.is_empty()).then(|| name.to_string())
        }
        _ if tag.starts_with("!ruby/string:") => {
            let name = &tag["!ruby/string:".len()..];
            (!name.is_empty()).then(|| name.to_string())
        }
        "!ruby/object:BigDecimal" => Some("BigDecimal".to_string()),
        "!ruby/object:DateTime" => Some("DateTime".to_string()),
        "!ruby/object:Complex" => Some("Complex".to_string()),
        "!ruby/object:Rational" => Some("Rational".to_string()),
        // `resolve_class o.value` — the scalar's own text is the class name.
        "!ruby/class" | "!ruby/module" => (!value.is_empty()).then(|| value.to_string()),
        "!ruby/regexp" => Some("Regexp".to_string()),
        "!ruby/range" => Some("Range".to_string()),
        // `/^!ruby\/sym(bol)?:?(.*)?$/` — `.*` swallows any tail after
        // `!ruby/sym`.
        _ if tag.starts_with("!ruby/sym") => Some("Symbol".to_string()),
        _ => tokenize_disallowed(value),
    }
}

/// `tokenize`'s ALPHA arm (`%r{^[^\d.:-]?[[:alpha:]_\s!@#$%\^&*(){}<>|/\\~;=]+}`)
/// — takes String/bool/nil spellings before the typed arms run. Ruby's
/// `\s` is ASCII-only (`[ \t\r\n\f\v]`); Rust's `\s` is Unicode-aware,
/// which would kill a timestamp-looking scalar separated by e.g. U+3000
/// (oracle loads it as a String — issue #347 review). Ruby's
/// `[[:alpha:]]` is Unicode; the port's `\p{Alpha}` is ASCII — every
/// value the two disagree on falls to the tokenizer's `else` String arm
/// either way, so the disagreement is invisible to every modeled arm.
fn alpha_match(s: &str) -> bool {
    static ALPHA: OnceLock<Regex> = OnceLock::new();
    ALPHA
        .get_or_init(|| {
            Regex::new(r"^[^\d.:-]?[\p{Alpha}_ \t\r\n\x0b\x0c!@#$%^&*(){}<>|/\\~;=]+").unwrap()
        })
        .is_match(s)
}

/// `tokenize`'s TIME arm — `class_loader.load 'Time'` dies before the
/// parse runs.
fn time_match(s: &str) -> bool {
    static TIME: OnceLock<Regex> = OnceLock::new();
    TIME
        .get_or_init(|| {
            Regex::new(
                r"^-?[0-9]{4}-[0-9]{1,2}-[0-9]{1,2}(?:[Tt]|[ \t\r\n\x0b\x0c]+)[0-9]{1,2}:[0-9]{2}:[0-9]{2}(?:\.[0-9]*)?(?:[ \t\r\n\x0b\x0c]*(?:Z|[-+][0-9]{1,2}:?(?:[0-9]{2})?))?$",
            )
            .unwrap()
        })
        .is_match(s)
}

/// `tokenize`'s DATE arm — validity unchecked (`2024-02-31` dies).
fn date_match(s: &str) -> bool {
    static DATE: OnceLock<Regex> = OnceLock::new();
    DATE
        .get_or_init(|| {
            Regex::new(r"^[0-9]{4}-(?:1[012]|0[0-9]|[0-9])-(?:[12][0-9]|3[01]|0[0-9]|[0-9])$")
                .unwrap()
        })
        .is_match(s)
}

/// `ScalarScanner`'s FLOAT shape — a dot is mandatory, digits optional on
/// both sides of it, the exponent signed.
fn psych_float_match(s: &str) -> bool {
    static FLOAT: OnceLock<Regex> = OnceLock::new();
    FLOAT
        .get_or_init(|| {
            Regex::new(r"^[-+]?(?:[0-9][0-9_,]*)?\.[0-9]*(?:[eE][-+][0-9]+)?$").unwrap()
        })
        .is_match(s)
}

/// The FLOAT arm's bare-dot escape (`\A[-+]?\.\Z` returns the String).
fn bare_dot_match(s: &str) -> bool {
    static BARE: OnceLock<Regex> = OnceLock::new();
    BARE
        .get_or_init(|| Regex::new(r"^[-+]?\.$").unwrap())
        .is_match(s)
}

/// What Ruby's `Float()` accepts over a FLOAT-arm candidate: digits around
/// an optional dot with an optional exponent (probe: `Float("1E5")` and
/// `Float("1e5")` load, `Float("e+1")` raises).
fn float_grammar_match(s: &str) -> bool {
    static GRAMMAR: OnceLock<Regex> = OnceLock::new();
    GRAMMAR
        .get_or_init(|| {
            Regex::new(r"^[+-]?(?:[0-9]+(?:\.[0-9]*)?|\.[0-9]+)(?:[eE][-+]?[0-9]+)?$").unwrap()
        })
        .is_match(s)
}

/// Does the tokenizer yield a FROZEN value for the plain scalar `s`?
/// `init_with`'s `instance_variable_set` raises `FrozenError` on frozen
/// results, so a `!str` mapping's `str` value freezes exactly when
/// `tokenize` answers nil/bool/Integer/Float: the ALPHA branch's
/// `~`/`null`/`yes|true|on`/`no|false|off` spellings (its other results
/// are Strings — `/^[^ytonf~]/i` takes anything else, including every
/// multi-byte spelling), the `.inf`/`.nan` floats, and the sexagesimal,
/// FLOAT and INTEGER arms. The Time/Date/Symbol arms die at the
/// restricted loader before any value exists — reported `true`, which
/// only ever declines.
fn tokenize_freezes(s: &str) -> bool {
    if alpha_match(s) || s.contains('\n') {
        if s.chars().count() > 5 {
            return false;
        }
        // `/^[^ytonf~]/i` — a first char outside the ytonf spellings
        // returns the String unfrozen.
        if s.chars().next().is_some_and(|c| !"yYtToOnNfF~".contains(c)) {
            return false;
        }
        s == "~"
            || s.eq_ignore_ascii_case("null")
            || matches!(
                s.to_ascii_lowercase().as_str(),
                "yes" | "true" | "on" | "no" | "false" | "off"
            )
    } else if (s.starts_with(':') && s.len() > 1) || time_match(s) || date_match(s) {
        // A restricted-loader death upstream — the walk ends before
        // `init_with`, so `true` only ever declines.
        true
    } else if inf_nan_match(s) || sexagesimal_match(s) || integer_match(s) {
        // `Float::INFINITY`/`NaN`, sexagesimal and INTEGER results —
        // all frozen.
        true
    } else if psych_float_match(s) {
        // The FLOAT arm's bare-dot escape returns the (unfrozen) String;
        // every other FLOAT result is a frozen Float — or the walk died
        // at `Float()` before `init_with`, where `true` only declines.
        !bare_dot_match(s)
    } else {
        // The `else` arm returns the (unfrozen) String.
        false
    }
}

/// `.inf`/`-.inf`/`.nan` — `tokenize` returns `Float::INFINITY`/`NAN`,
/// both frozen.
fn inf_nan_match(s: &str) -> bool {
    static INF_NAN: OnceLock<Regex> = OnceLock::new();
    INF_NAN
        .get_or_init(|| Regex::new(r"(?i)^[+-]?\.inf$|^\.nan$").unwrap())
        .is_match(s)
}

/// The sexagesimal arms — Integer/Float results, both frozen.
fn sexagesimal_match(s: &str) -> bool {
    static SEXA: OnceLock<Regex> = OnceLock::new();
    SEXA
        .get_or_init(|| {
            Regex::new(r"^[-+]?[0-9][0-9_]*(?::[0-5]?[0-9]){1,2}(?:\.[0-9_]*)?$").unwrap()
        })
        .is_match(s)
}

/// `tokenize`'s INTEGER arm (`INTEGER_LEGACY`, the default
/// `strict_integer: false`) — Integer results, frozen.
fn integer_match(s: &str) -> bool {
    static INTEGER: OnceLock<Regex> = OnceLock::new();
    INTEGER
        .get_or_init(|| {
            Regex::new(
                r"^[+-]?(?:0b[_,]*[0-1][0-1_,]*|0[_,]*[0-7][0-7_,]*|(?:0|[1-9](?:[0-9]|,[0-9]|_[0-9])*)|0x[_,]*[0-9a-fA-F][0-9a-fA-F_,]*)$",
            )
            .unwrap()
        })
        .is_match(s)
}

/// The `ScalarScanner#tokenize` arms that die under `permitted_classes: []`:
/// TIME's `parse_time` loads `Time`, the date arm `Date`, and the `:sym` arm
/// `Symbol` — everything else lands on a class `safe_load` always permits
/// (and `Float(...)`/`Integer(...)` never consult the loader).
fn tokenize_disallowed(s: &str) -> Option<String> {
    if s.is_empty() || s.contains('\n') || alpha_match(s) {
        return None;
    }
    if time_match(s) {
        return Some("Time".to_string());
    }
    if date_match(s) {
        return Some("Date".to_string());
    }
    // `/^:./` — the `parse_symbols` arm (default on). The `.inf`/`-.inf`/`.nan`
    // arms before it land on a permitted Float, and no `:`-led scalar reaches
    // them anyway.
    if s.len() > 1 && s.starts_with(':') {
        return Some("Symbol".to_string());
    }
    None
}

/// A libyaml error string, `None` for NULL (Psych's `nil`).
///
/// # Safety
/// `ptr` is NULL or a NUL-terminated string.
unsafe fn c_string(ptr: *const i8) -> Option<String> {
    if ptr.is_null() {
        return None;
    }
    // SAFETY: the caller's contract.
    Some(
        unsafe { CStr::from_ptr(ptr.cast()) }
            .to_string_lossy()
            .into_owned(),
    )
}
