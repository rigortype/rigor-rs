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

use std::ffi::CStr;
use std::mem::MaybeUninit;

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
                    out = scalar_rejection(tag.as_deref(), &value, scalar.quoted_implicit);
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
                    }
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
        // child death the scan records later is one the visitor reaches.
        _ => None,
    }
}

/// `Psych::DisallowedClass` for one scalar event, if `safe_load`'s visitor
/// raises it: `ToRuby#deserialize` returns `o.value` untouched only when the
/// scalar is QUOTED — and `Nodes::Scalar#quoted` is libyaml's
/// `quoted_implicit`, which is FALSE whenever a tag is written (probe:
/// `!foo "2024-01-01"` is `quoted=false` on `Psych.parse` and dies `Date`
/// upstream). An untagged plain scalar goes to `ScalarScanner#tokenize`;
/// explicit tags take `deserialize`'s `case o.tag` arms.
fn scalar_rejection(
    tag: Option<&str>,
    value: &str,
    quoted: bool,
) -> Option<SafeLoadRejection> {
    if quoted {
        return None;
    }
    disallowed_class(tag, value).map(SafeLoadRejection::DisallowedClass)
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
        // `!ruby/encoding` calls `Encoding.find` (loads the Encoding, or an
        // `ArgumentError` — an uncaught shape serde_yaml's own tag rejection
        // already approximates; not modelled).
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

/// The `ScalarScanner#tokenize` arms that die under `permitted_classes: []`:
/// TIME's `parse_time` loads `Time`, the date arm `Date`, and the `:sym` arm
/// `Symbol` — everything else lands on a class `safe_load` always permits
/// (and `Float(...)`/`Integer(...)` never consult the loader).
fn tokenize_disallowed(s: &str) -> Option<String> {
    use regex::Regex;
    use std::sync::OnceLock;
    // `tokenize`'s first arm — `%r{^[^\d.:-]?[[:alpha:]_\s!@#$%\^&*(){}<>|/\\~;=]+}`
    // — takes String/bool/nil spellings before the typed arms run.
    static ALPHA: OnceLock<Regex> = OnceLock::new();
    static TIME: OnceLock<Regex> = OnceLock::new();
    static DATE: OnceLock<Regex> = OnceLock::new();
    // Ruby's `\s` is ASCII-only (`[ \t\r\n\f\v]`); Rust's `\s` is
    // Unicode-aware, which would kill a timestamp-looking scalar separated
    // by e.g. U+3000 (oracle loads it as a String — issue #347 review).
    let alpha = ALPHA.get_or_init(|| {
        Regex::new(r"^[^\d.:-]?[\p{Alpha}_ \t\r\n\x0b\x0c!@#$%^&*(){}<>|/\\~;=]+").unwrap()
    });
    let time = TIME.get_or_init(|| {
        Regex::new(
            r"^-?[0-9]{4}-[0-9]{1,2}-[0-9]{1,2}(?:[Tt]|[ \t\r\n\x0b\x0c]+)[0-9]{1,2}:[0-9]{2}:[0-9]{2}(?:\.[0-9]*)?(?:[ \t\r\n\x0b\x0c]*(?:Z|[-+][0-9]{1,2}:?(?:[0-9]{2})?))?$",
        )
        .unwrap()
    });
    let date = DATE.get_or_init(|| {
        Regex::new(r"^[0-9]{4}-(?:1[012]|0[0-9]|[0-9])-(?:[12][0-9]|3[01]|0[0-9]|[0-9])$")
            .unwrap()
    });
    if s.is_empty() || s.contains('\n') || alpha.is_match(s) {
        return None;
    }
    if time.is_match(s) {
        return Some("Time".to_string());
    }
    if date.is_match(s) {
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
