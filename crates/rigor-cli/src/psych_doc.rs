//! `YAML.safe_load_file(path, permitted_classes: [Symbol])` for the baseline
//! loader (issue #327).
//!
//! The baseline file is interchangeable with the reference's
//! `.rigor-baseline.yml`, which `Rigor::Analysis::Baseline.load` reads with a
//! full `Psych.safe_load_file`. Accepting exactly the YAML libyaml accepts —
//! and dying where `safe_load` dies — requires the same parser, so this
//! module drives `unsafe-libyaml` (the crate `serde_yaml` wraps; Psych drives
//! the same C library) at the EVENT level and replays Psych's
//! `ToRuby`/`NoAliasRuby` visitor over the resulting document tree.
//!
//! Three phases, in upstream order:
//!
//! 1. **Parse** — libyaml events for the FIRST document only
//!    (`YAML.safe_load` → `Psych.parse` returns after document 1; a syntax
//!    error inside a later document is never seen). A parser error anywhere
//!    inside document 1 is a `Psych::SyntaxError` and beats every visitor
//!    death: `safe_load` parses the document whole, then walks it.
//! 2. **Visit** — `NoAliasRuby` raises `Psych::AliasesNotEnabled` on any
//!    ALIAS node in document order; scalar nodes type through Psych's
//!    `ScalarScanner`/`deserialize` arms (`crate::baseline::classify_scalar`)
//!    and collection tags resolve through the restricted
//!    `ClassLoader` (`permitted_classes: [Symbol]`) before, during, or after
//!    their children exactly as the corresponding `to_ruby.rb` arm does.
//! 3. **Merge/validate** — `revive_hash` applies `<<` merge keys and
//!    last-wins duplicate keys with Ruby `eql?` semantics; the caller
//!    (`Baseline::parse`) then runs the `parse_loaded`/`parse_row` shape
//!    checks in upstream order.

use std::ffi::CStr;
use std::mem::MaybeUninit;

use crate::baseline::PsychScalar;

/// A libyaml node of the baseline document, before `ToRuby` typing.
pub(crate) enum Raw {
    /// A scalar: `value` is the cooked content (libyaml already applied
    /// quoting escapes, folding and chomping); `tag` is the resolved tag
    /// string (`!!str` arrives as `tag:yaml.org,2002:str`); `quoted` is
    /// libyaml's `quoted_implicit` — the flag `Nodes::Scalar#quoted` exposes,
    /// false whenever a tag is written.
    Scalar {
        value: String,
        tag: Option<String>,
        quoted: bool,
    },
    /// An alias — `NoAliasRuby#visit_Psych_Nodes_Alias` raises on sight.
    Alias,
    Seq {
        tag: Option<String>,
        items: Vec<Raw>,
    },
    Map {
        tag: Option<String>,
        pairs: Vec<(Raw, Raw)>,
    },
}

impl Raw {
    fn tag(&self) -> Option<&str> {
        match self {
            Raw::Scalar { tag, .. } | Raw::Seq { tag, .. } | Raw::Map { tag, .. } => {
                tag.as_deref()
            }
            Raw::Alias => None,
        }
    }
}

/// A `Psych::SyntaxError` — the libyaml parser error document 1 stops on.
/// `line`/`column` are `context_mark + 1`, as `psych_parser.c`'s
/// `make_exception` reports them.
pub(crate) struct SyntaxError {
    pub line: usize,
    pub column: usize,
    pub problem: String,
    pub context: Option<String>,
}

impl SyntaxError {
    /// `(file): problem context at line L column C` — the `SyntaxError`
    /// message text (`psych/syntax_error.rb`).
    pub(crate) fn message(&self, label: &str) -> String {
        match &self.context {
            Some(ctx) => format!(
                "({label}): {} {ctx} at line {} column {}",
                self.problem, self.line, self.column
            ),
            None => format!(
                "({label}): {} at line {} column {}",
                self.problem, self.line, self.column
            ),
        }
    }
}

/// A loaded baseline value — what `YAML.safe_load_file` hands to
/// `parse_loaded`. `Scalar` covers the String/Integer/Float/bool/nil/Symbol
/// atoms plus `PsychScalar::Opaque` objects; collections are the
/// `revive_hash`/`register_empty` results with merges and last-wins
/// duplicate keys already applied.
pub(crate) enum Loaded {
    Scalar(PsychScalar),
    Seq(Vec<Loaded>),
    Map(Vec<(Loaded, Loaded)>),
}

// =========================================================================
// Phase 1 — libyaml event parse of document 1
// =========================================================================

/// A collection still open on the event stack.
enum Partial {
    Seq {
        tag: Option<String>,
        items: Vec<Raw>,
    },
    Map {
        tag: Option<String>,
        pairs: Vec<(Raw, Raw)>,
        /// The key awaiting its value.
        pending: Option<Raw>,
    },
}

impl Partial {
    fn push_node(&mut self, node: Raw) {
        match self {
            Partial::Seq { items, .. } => items.push(node),
            Partial::Map { pairs, pending, .. } => {
                if let Some(k) = pending.take() {
                    pairs.push((k, node));
                } else {
                    *pending = Some(node);
                }
            }
        }
    }
}

/// Parse the first document of `text`. `Ok` is its root node — always `Some`
/// in practice (libyaml emits an empty plain scalar for an empty document,
/// but the caller treats `None` as one anyway).
///
/// # Safety note
/// Same libyaml contract as `config::psych_error`: the boxed parser owns a
/// pointer to itself, `text` outlives it, every event is deleted after it is
/// read, and the error strings are copied out before `yaml_parser_delete`.
pub(crate) fn parse_first_document(text: &str) -> Result<Raw, SyntaxError> {
    let mut parser = Box::new(MaybeUninit::<unsafe_libyaml::yaml_parser_t>::uninit());
    unsafe {
        let parser = parser.as_mut_ptr();
        if unsafe_libyaml::yaml_parser_initialize(parser).fail {
            // Memory error — outside the modeled surface; the caller sees a
            // SyntaxError-shaped fatal like any other parse death.
            return Err(SyntaxError {
                line: 0,
                column: 0,
                problem: "control flow error".to_string(),
                context: None,
            });
        }
        unsafe_libyaml::yaml_parser_set_encoding(parser, unsafe_libyaml::YAML_UTF8_ENCODING);
        unsafe_libyaml::yaml_parser_set_input_string(parser, text.as_ptr(), text.len() as u64);

        let mut stack: Vec<Partial> = Vec::new();
        let mut root: Option<Raw> = None;
        let mut error = None;
        loop {
            let mut event = MaybeUninit::<unsafe_libyaml::yaml_event_t>::uninit();
            if unsafe_libyaml::yaml_parser_parse(parser, event.as_mut_ptr()).fail {
                let p = &*parser;
                error = Some(SyntaxError {
                    line: p.context_mark.line as usize + 1,
                    column: p.context_mark.column as usize + 1,
                    problem: c_string(p.problem).unwrap_or_default(),
                    context: c_string(p.context),
                });
                unsafe_libyaml::yaml_event_delete(event.as_mut_ptr());
                break;
            }
            let ev = event.as_ptr();
            let kind = (*ev).type_;
            match kind {
                unsafe_libyaml::YAML_SCALAR_EVENT => {
                    let s = &(*ev).data.scalar;
                    let node = Raw::Scalar {
                        value: bytes_string(s.value, s.length),
                        tag: c_string(s.tag.cast()),
                        // `Nodes::Scalar#quoted` is libyaml's
                        // `quoted_implicit` verbatim — true for quoted and
                        // block scalars, false for plain and tagged ones
                        // (probed through `Psych.parse`).
                        quoted: s.quoted_implicit,
                    };
                    push(&mut stack, &mut root, node);
                }
                unsafe_libyaml::YAML_ALIAS_EVENT => {
                    push(&mut stack, &mut root, Raw::Alias);
                }
                unsafe_libyaml::YAML_SEQUENCE_START_EVENT => {
                    let tag = c_string((*ev).data.sequence_start.tag.cast());
                    stack.push(Partial::Seq {
                        tag,
                        items: Vec::new(),
                    });
                }
                unsafe_libyaml::YAML_MAPPING_START_EVENT => {
                    let tag = c_string((*ev).data.mapping_start.tag.cast());
                    stack.push(Partial::Map {
                        tag,
                        pairs: Vec::new(),
                        pending: None,
                    });
                }
                unsafe_libyaml::YAML_SEQUENCE_END_EVENT => {
                    if let Some(Partial::Seq { tag, items }) = stack.pop() {
                        push(&mut stack, &mut root, Raw::Seq { tag, items });
                    }
                }
                unsafe_libyaml::YAML_MAPPING_END_EVENT => {
                    if let Some(Partial::Map {
                        tag,
                        pairs,
                        pending: _,
                    }) = stack.pop()
                    {
                        push(&mut stack, &mut root, Raw::Map { tag, pairs });
                    }
                }
                unsafe_libyaml::YAML_DOCUMENT_END_EVENT
                | unsafe_libyaml::YAML_STREAM_END_EVENT => {
                    unsafe_libyaml::yaml_event_delete(event.as_mut_ptr());
                    break;
                }
                _ => {}
            }
            unsafe_libyaml::yaml_event_delete(event.as_mut_ptr());
        }
        unsafe_libyaml::yaml_parser_delete(parser);

        if let Some(e) = error {
            return Err(e);
        }
        // An empty document arrives as a plain empty scalar (or, defensively,
        // no root at all) — `safe_load` yields `nil` for both.
        Ok(root.unwrap_or(Raw::Scalar {
            value: String::new(),
            tag: None,
            quoted: false,
        }))
    }
}

fn push(stack: &mut Vec<Partial>, root: &mut Option<Raw>, node: Raw) {
    match stack.last_mut() {
        Some(partial) => partial.push_node(node),
        None => *root = Some(node),
    }
}

/// A libyaml `value`/`length` byte pair as a String (lossy on invalid UTF-8 —
/// `read_to_string` already rejected non-UTF-8 files).
fn bytes_string(ptr: *const u8, len: u64) -> String {
    if ptr.is_null() {
        return String::new();
    }
    unsafe { String::from_utf8_lossy(std::slice::from_raw_parts(ptr, len as usize)).into_owned() }
}

/// A libyaml error/tag string, `None` for NULL.
///
/// # Safety
/// `ptr` is NULL or a NUL-terminated string owned by libyaml; the text is
/// copied out before the event/parser is deleted.
unsafe fn c_string(ptr: *const i8) -> Option<String> {
    if ptr.is_null() {
        return None;
    }
    Some(
        unsafe { CStr::from_ptr(ptr.cast()) }
            .to_string_lossy()
            .into_owned(),
    )
}

// =========================================================================
// Phase 2 — `NoAliasRuby`/`ToRuby` visitor
// =========================================================================

/// `YAML.safe_load_file`'s whole result for `text`: a `Psych::SyntaxError`
/// from document 1, or the first visitor death in document order, or the
/// loaded value. `Err` strings are the port's `<Class>: <message>`
/// [`crate::baseline::LoadError::fatal`] convention — the crash shape the
/// reference shows as an uncaught exception.
pub(crate) fn load(text: &str, label: &str) -> Result<Loaded, String> {
    let raw = parse_first_document(text)
        .map_err(|e| format!("Psych::SyntaxError: {}", e.message(label)))?;
    visit(&raw)
}

/// `accept` one node — `NoAliasRuby#visit_Psych_Nodes_Alias` first, then the
/// kind's arm.
fn visit(node: &Raw) -> Result<Loaded, String> {
    match node {
        Raw::Alias => Err("Psych::AliasesNotEnabled: Alias parsing was not enabled. \
                          To enable it, pass `aliases: true` to `Psych::load` or \
                          `Psych::safe_load`."
            .to_string()),
        Raw::Scalar {
            value, tag, quoted,
        } => Ok(Loaded::Scalar(crate::baseline::classify_scalar(
            tag.as_deref(),
            value,
            !*quoted,
        )?)),
        Raw::Seq { tag, items } => visit_seq(tag.as_deref(), items),
        Raw::Map { tag, pairs } => visit_map(tag.as_deref(), pairs),
    }
}

/// `Psych::DisallowedClass` — `Tried to load unspecified class: X`.
fn disallowed(name: &str) -> String {
    format!("Psych::DisallowedClass: Tried to load unspecified class: {name}")
}

/// `visit_Psych_Nodes_Sequence`: `!omap` and `/^!(?:seq|ruby\/array):(.*)$/`
/// have their own arms; every other tag falls to `register_empty` — a plain
/// Array whose children still visit in order.
fn visit_seq(tag: Option<&str>, items: &[Raw]) -> Result<Loaded, String> {
    match tag {
        Some("!omap") | Some("tag:yaml.org,2002:omap") => {
            // `Psych::Omap.new` needs no class permission; each element then
            // contributes `accept(children.first) => accept(children.last)` —
            // a scalar or alias element has no children (`nil.first` dies),
            // an empty one `accept`s nil (`nil.accept` dies).
            let mut pairs: Vec<(Loaded, Loaded)> = Vec::new();
            for item in items {
                let (first, last) = match item {
                    Raw::Seq { items, .. } => (items.first(), items.last()),
                    Raw::Map { pairs, .. } => (
                        pairs.first().map(|(k, _)| k),
                        pairs.last().map(|(_, v)| v),
                    ),
                    Raw::Scalar { .. } | Raw::Alias => {
                        return Err(
                            "NoMethodError: undefined method `first' for nil".to_string()
                        );
                    }
                };
                let (Some(k), Some(v)) = (first, last) else {
                    return Err(
                        "NoMethodError: undefined method `accept' for nil".to_string()
                    );
                };
                let key = visit(k)?;
                let val = visit(v)?;
                insert(&mut pairs, key, val);
            }
            Ok(Loaded::Map(pairs))
        }
        Some(t) => {
            for prefix in ["!seq:", "!ruby/array:"] {
                if let Some(name) = t.strip_prefix(prefix) {
                    // `resolve_class($1)` runs BEFORE the children: an empty
                    // name is `nil.allocate`, `Symbol` allocates (TypeError),
                    // anything else dies `DisallowedClass`.
                    return Err(if name.is_empty() {
                        "NoMethodError: undefined method `allocate' for nil".to_string()
                    } else if name == "Symbol" {
                        "TypeError: allocator undefined for Symbol".to_string()
                    } else {
                        disallowed(name)
                    });
                }
            }
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                out.push(visit(item)?);
            }
            Ok(Loaded::Seq(out))
        }
        None => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                out.push(visit(item)?);
            }
            Ok(Loaded::Seq(out))
        }
    }
}

/// `visit_Psych_Nodes_Mapping` — `to_ruby.rb`'s `case o.tag`. Arms that call
/// `resolve_class`/`class_loader.*` die BEFORE their children visit; arms
/// that walk children first (`!ruby/exception`, `!ruby/data`, the `!str`
/// family, `!ruby/hash-with-ivars`) are modeled in that order. Any
/// non-arm tag falls to `else` — a plain `revive_hash`.
fn visit_map(tag: Option<&str>, pairs: &[(Raw, Raw)]) -> Result<Loaded, String> {
    let Some(t) = tag else {
        return revive_hash(pairs);
    };
    // `/^!map:(.*)$/` / `/^!ruby\/hash:(.*)$/` — `resolve_class($1).allocate`
    // before children.
    for prefix in ["!map:", "!ruby/hash:"] {
        if let Some(name) = t.strip_prefix(prefix) {
            return Err(allocate_or_disallowed(name));
        }
    }
    match t {
        "!omap" | "tag:yaml.org,2002:omap" => Err(disallowed("Psych::Omap")),
        "!set" | "tag:yaml.org,2002:set" => Err(disallowed("Psych::Set")),
        "!ruby/range" => Err(disallowed("Range")),
        // `/^!ruby\/marshalable:(.*)$/` — `resolve_class(name).allocate`
        // before children.
        _ if t.starts_with("!ruby/marshalable:") => {
            Err(allocate_or_disallowed(&t["!ruby/marshalable:".len()..]))
        }
        // `/^!ruby\/struct:?(.*)?$/` — the colon is OPTIONAL upstream
        // (`!ruby/structure` names `ure`). `nil`/`""` names skip
        // `resolve_class` and land on `class_loader.struct`.
        _ if t.starts_with("!ruby/struct") => {
            let name = t["!ruby/struct".len()..].strip_prefix(':').unwrap_or(
                &t["!ruby/struct".len()..],
            );
            Err(if name.is_empty() {
                disallowed("Struct")
            } else if name == "Symbol" {
                "TypeError: allocator undefined for Symbol".to_string()
            } else {
                disallowed(name)
            })
        }
        // `/^!ruby\/object:?(.*)?$/` — `name = $1 || 'Object'`; `Hash`
        // revives without a permission check, `Complex`/`Rational` resolve
        // through the loader first.
        _ if t.starts_with("!ruby/object") => {
            let rest = &t["!ruby/object".len()..];
            let name = rest.strip_prefix(':').unwrap_or(rest);
            match name {
                "Complex" => Err(disallowed("Complex")),
                "Rational" => Err(disallowed("Rational")),
                "Hash" => revive_hash(pairs),
                "Symbol" => Err("TypeError: allocator undefined for Symbol".to_string()),
                "" => Err(disallowed("Object")),
                _ => Err(disallowed(name)),
            }
        }
        // `/^!ruby\/data(-with-ivars)?(?::(.*))?$/` — a named class dies
        // before children; unnamed walks the `members`/`ivars` members and
        // dies `Data` after them.
        _ if t.starts_with("!ruby/data") => visit_data_map(t, pairs),
        // `/^!(?:str|ruby\/string)(?::(.*))?$/` + `tag:yaml.org,2002:str` —
        // `resolve_class($1)` first, then children, then `init_with`.
        _ if is_str_map_tag(t) => visit_str_map(t, pairs),
        // `/^!ruby\/array:(.*)$/` — `klass.allocate` before children.
        _ if t.starts_with("!ruby/array:") => {
            Err(allocate_or_disallowed(&t["!ruby/array:".len()..]))
        }
        // `/^!ruby\/exception:?(.*)?$/` — children visit FIRST
        // (`Hash[*children.map accept]`), then `resolve_class($1)` or
        // `class_loader.exception`.
        _ if t.starts_with("!ruby/exception") => {
            for (k, v) in pairs {
                visit(k)?;
                visit(v)?;
            }
            let rest = &t["!ruby/exception".len()..];
            let name = rest.strip_prefix(':').unwrap_or(rest);
            Err(if name.is_empty() {
                disallowed("Exception")
            } else if name == "Symbol" {
                // `build_exception(Symbol, …)` → `Symbol.new(msg)`.
                "NoMethodError: undefined method `new' for Symbol".to_string()
            } else {
                disallowed(name)
            })
        }
        // `/^!ruby\/hash-with-ivars(?::(.*))?$/` — a named class dies before
        // children; unnamed visits only `elements`/`ivars` member values.
        _ if t.starts_with("!ruby/hash-with-ivars") => visit_ivars_map(t, pairs),
        _ => revive_hash(pairs),
    }
}

/// `resolve_class(name)` then `.allocate` — the shared death shape of the
/// `!map:`/`!ruby/hash:`/`!ruby/array:`/`!ruby/marshalable:` arms: empty
/// name → `nil.allocate`, `Symbol` → `TypeError`, anything else
/// `DisallowedClass`.
fn allocate_or_disallowed(name: &str) -> String {
    if name.is_empty() {
        "NoMethodError: undefined method `allocate' for nil".to_string()
    } else if name == "Symbol" {
        "TypeError: allocator undefined for Symbol".to_string()
    } else {
        disallowed(name)
    }
}

/// `/^!(?:str|ruby\/string)(?::(.*))?$/` — `!str`/`!ruby/string`, each
/// optionally `:K`-named, and `tag:yaml.org,2002:str`. `!strFoo` does NOT
/// match (the name needs a colon).
fn is_str_map_tag(t: &str) -> bool {
    t == "tag:yaml.org,2002:str" || t == "!str" || t == "!ruby/string" ||
        t.starts_with("!str:") || t.starts_with("!ruby/string:")
}

/// The `!str`-family mapping arm: `resolve_class($1)` runs first (a named
/// class dies here); children then visit in order, the `str` member decides
/// the string, and `init_with` runs `instance_variable_set` for every
/// non-`str` member — a `FrozenError` on an immediate receiver (nil, the
/// numerics, bool, Symbol) or a `NameError` on an invalid ivar name.
fn visit_str_map(t: &str, pairs: &[(Raw, Raw)]) -> Result<Loaded, String> {
    // `klass = resolve_class($1)` — `!str:K`/`!ruby/string:K` resolve BEFORE
    // children; `Symbol` alone is permitted and defers its death to the
    // `str` member's `klass.allocate`.
    let name = t
        .strip_prefix("!str:")
        .or_else(|| t.strip_prefix("!ruby/string:"));
    let symbol_klass = match name {
        Some("") | None => false,
        Some("Symbol") => true,
        Some(other) => return Err(disallowed(other)),
    };

    let mut string: Option<Loaded> = None;
    let mut members: Vec<Loaded> = Vec::new();
    for (k, v) in pairs {
        let key = visit(k)?;
        let val = visit(v)?;
        // `key == 'str'` — the TYPED key, so `:str` does not match.
        if matches!(&key, Loaded::Scalar(PsychScalar::Str(s)) if s == "str") {
            if symbol_klass {
                return Err("TypeError: allocator undefined for Symbol".to_string());
            }
            string = Some(val);
        } else {
            members.push(key);
        }
    }
    if members.is_empty() {
        return Ok(string.unwrap_or(Loaded::Scalar(PsychScalar::Nil)));
    }
    // `init_with`: `o.instance_variable_set` per member — a frozen receiver
    // dies first (an absent `str` member leaves `string` nil, frozen too);
    // an invalid `@name` dies `NameError`.
    for member_key in &members {
        if let Some(err) = frozen_receiver(&string) {
            return Err(err);
        }
        let name = format!("@{}", member_key.to_s());
        if !valid_ivar_name(&name) {
            return Err(format!(
                "NameError: '{name}' is not allowed as an instance variable name"
            ));
        }
    }
    Ok(string.unwrap_or(Loaded::Scalar(PsychScalar::Nil)))
}

/// The `FrozenError` `instance_variable_set` raises on an immediate
/// receiver — including an ABSENT `str` member, which leaves `string` as
/// `nil` (`can't modify frozen NilClass: nil`, probed). `None` for a
/// receiver that can take ivars (String/Array/Hash/allocated objects).
fn frozen_receiver(string: &Option<Loaded>) -> Option<String> {
    let loaded = match string {
        Some(l) => l,
        None => {
            return Some(
                "FrozenError: can't modify frozen NilClass: nil".to_string()
            );
        }
    };
    let (class, inspect) = match loaded {
        Loaded::Scalar(PsychScalar::Nil) => ("NilClass", "nil".to_string()),
        Loaded::Scalar(PsychScalar::Bool(b)) => {
            (if *b { "TrueClass" } else { "FalseClass" }, b.to_string())
        }
        Loaded::Scalar(PsychScalar::Int(i)) => ("Integer", i.digits.clone()),
        Loaded::Scalar(PsychScalar::Float(_)) => {
            ("Float", loaded.to_s())
        }
        Loaded::Scalar(PsychScalar::Symbol(_)) => ("Symbol", loaded.inspect()),
        _ => return None,
    };
    Some(format!("FrozenError: can't modify frozen {class}: {inspect}"))
}

/// `@name` is a Ruby instance-variable name — `@[a-zA-Z_]\w*`.
fn valid_ivar_name(name: &str) -> bool {
    let mut chars = name.chars();
    if chars.next() != Some('@') {
        return false;
    }
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// `/^!ruby\/data(-with-ivars)?(?::(.*))?$/`: `!ruby/data:K`/
/// `!ruby/data-with-ivars:K` resolve before children (`nil.allocate` for an
/// empty name); bare `-with-ivars` walks `members`/`ivars` pairs and bare
/// `!ruby/data` treats the map itself as members — both die
/// `DisallowedClass: Data` afterwards (the loader has no `Data`).
fn visit_data_map(t: &str, pairs: &[(Raw, Raw)]) -> Result<Loaded, String> {
    let rest = &t["!ruby/data".len()..];
    if let Some(after) = rest.strip_prefix('-') {
        // `!ruby/data-with-ivars[:K]`
        let Some(with_ivars) = after.strip_prefix("with-ivars") else {
            // `!ruby/data-foo` — the regex does not match: plain hash.
            return revive_hash(pairs);
        };
        if let Some(name) = with_ivars.strip_prefix(':') {
            return Err(allocate_or_disallowed(name));
        }
        if !with_ivars.is_empty() {
            return revive_hash(pairs);
        }
        for (k, v) in pairs {
            let key = visit(k)?;
            match key {
                Loaded::Scalar(PsychScalar::Str(ref s)) if s == "members" || s == "ivars" => {
                    visit_member_pairs(v)?;
                }
                _ => {}
            }
        }
        return Err(disallowed("Data"));
    }
    if let Some(name) = rest.strip_prefix(':') {
        return Err(allocate_or_disallowed(name));
    }
    if !rest.is_empty() {
        return revive_hash(pairs);
    }
    // `!ruby/data` — `revive_data_members(members, o)` on the map itself:
    // every pair is a member (`hash[symbolize(k)] = v`), then `Data` dies.
    // `accept(k)`, `accept(v)`, THEN `symbolize` — a non-String member name
    // dies `to_sym` after both children visited.
    for (k, v) in pairs {
        let key = visit(k)?;
        visit(v)?;
        if !matches!(key, Loaded::Scalar(PsychScalar::Str(_))) {
            return Err(format!(
                "NoMethodError: undefined method `to_sym' for {}",
                key.inspect()
            ));
        }
    }
    Err(disallowed("Data"))
}

/// `revive_data_members`/`revive_hash` over a `members:`/`ivars:` value —
/// the value must be a collection (`nil.each_slice` dies otherwise); its
/// pairs visit in order.
fn visit_member_pairs(node: &Raw) -> Result<(), String> {
    match node {
        Raw::Map { pairs, .. } => {
            for (k, v) in pairs {
                visit(k)?;
                visit(v)?;
            }
            Ok(())
        }
        Raw::Seq { items, .. } => {
            // `children.each_slice(2)` pairs the items off; an odd tail
            // `accept`s nil.
            for chunk in items.chunks(2) {
                visit(&chunk[0])?;
                match chunk.get(1) {
                    Some(v) => {
                        visit(v)?;
                    }
                    None => {
                        return Err(
                            "NoMethodError: undefined method `accept' for nil".to_string(),
                        );
                    }
                }
            }
            Ok(())
        }
        Raw::Scalar { .. } | Raw::Alias => {
            Err("NoMethodError: undefined method `each_slice' for nil".to_string())
        }
    }
}

/// `/^!ruby\/hash-with-ivars(?::(.*))?$/` — the unnamed arm keeps only the
/// `elements` member's hash (`ivars` members visit but don't survive into
/// the value the baseline loader sees); every other member is skipped
/// entirely — its value never visits.
fn visit_ivars_map(t: &str, pairs: &[(Raw, Raw)]) -> Result<Loaded, String> {
    let rest = &t["!ruby/hash-with-ivars".len()..];
    if let Some(name) = rest.strip_prefix(':') {
        return Err(allocate_or_disallowed(name));
    }
    if !rest.is_empty() {
        return revive_hash(pairs);
    }
    let mut elements = Vec::new();
    for (k, v) in pairs {
        // `case key.value` — the RAW key text, not the typed value.
        let raw_key = match k {
            Raw::Scalar { value, .. } => value.as_str(),
            _ => "",
        };
        match raw_key {
            "elements" => {
                // `revive_hash hash, value` — `value.children.each_slice(2)`:
                // a non-collection value has no children (`nil.each_slice`
                // dies), a sequence's children pair off two by two (an odd
                // tail `accept`s nil).
                let items: Vec<&Raw> = match v {
                    Raw::Map { pairs, .. } => {
                        pairs.iter().flat_map(|(k, v)| [k, v]).collect()
                    }
                    Raw::Seq { items, .. } => items.iter().collect(),
                    Raw::Scalar { .. } | Raw::Alias => {
                        return Err(
                            "NoMethodError: undefined method `each_slice' for nil"
                                .to_string(),
                        );
                    }
                };
                for chunk in items.chunks(2) {
                    let key = visit(chunk[0])?;
                    match chunk.get(1) {
                        Some(ev) => {
                            let val = visit(ev)?;
                            insert(&mut elements, key, val);
                        }
                        // An odd tail `accept`s nil as the pair's value.
                        None => {
                            return Err(
                                "NoMethodError: undefined method `accept' for nil"
                                    .to_string(),
                            );
                        }
                    }
                }
            }
            "ivars" => {
                visit_member_pairs(v)?;
            }
            _ => {}
        }
    }
    Ok(Loaded::Map(elements))
}

// =========================================================================
// Phase 3 — `revive_hash` (duplicates + `<<` merge) and Ruby equality
// =========================================================================

/// `revive_hash` — pair order with `<<` merges and last-wins `eql?` keys.
fn revive_hash(pairs: &[(Raw, Raw)]) -> Result<Loaded, String> {
    let mut out: Vec<(Loaded, Loaded)> = Vec::new();
    for (k, v) in pairs {
        let key = visit(k)?;
        let val = visit(v)?;
        // `key == '<<' && k.tag != "tag:yaml.org,2002:str"` — the raw node
        // tag, so a plain or `!str`-tagged `<<` merges but `!!str '<<'`
        // stays a literal key.
        let merge = matches!(&key, Loaded::Scalar(PsychScalar::Str(s)) if s == "<<")
            && k.tag() != Some("tag:yaml.org,2002:str");
        if !merge {
            insert(&mut out, key, val);
            continue;
        }
        match (v, val) {
            // `hash.merge! val` — a mapping value merges its loaded Hash;
            // one that loaded as something else (a `!str` map's String)
            // TypeErrors into `hash[key] = val`.
            (Raw::Map { .. }, Loaded::Map(ps)) => merge_pairs(&mut out, ps),
            (Raw::Map { .. }, other) => insert(&mut out, key, other),
            // A sequence merges each element REVERSED into a fresh hash,
            // TypeError (any non-Hash element) → `hash[key] = val`.
            (Raw::Seq { .. }, Loaded::Seq(items)) => {
                if items.iter().all(|i| matches!(i, Loaded::Map(_))) {
                    let mut h: Vec<(Loaded, Loaded)> = Vec::new();
                    for item in items.into_iter().rev() {
                        if let Loaded::Map(ps) = item {
                            merge_pairs(&mut h, ps);
                        }
                    }
                    merge_pairs(&mut out, h);
                } else {
                    insert(&mut out, key, Loaded::Seq(items));
                }
            }
            (Raw::Seq { .. }, other) => insert(&mut out, key, other),
            // Scalars and (dead) aliases take `hash[key] = val`.
            (_, other) => insert(&mut out, key, other),
        }
    }
    Ok(Loaded::Map(out))
}

/// `hash[key] = val` — replace the value of an `eql?` key, else append.
fn insert(out: &mut Vec<(Loaded, Loaded)>, key: Loaded, val: Loaded) {
    if let Some(slot) = out.iter_mut().find(|(ek, _)| ruby_eql(ek, &key)) {
        slot.1 = val;
    } else {
        out.push((key, val));
    }
}

/// `hash.merge!(other)` — every pair of `other` `insert`ed in order.
fn merge_pairs(out: &mut Vec<(Loaded, Loaded)>, pairs: Vec<(Loaded, Loaded)>) {
    for (k, v) in pairs {
        insert(out, k, v);
    }
}

/// Ruby `eql?` — the equality `Hash` keys and `==` comparisons in
/// `parse_row` see. Different kinds are never equal; an `Opaque` object
/// (two Encodings, a str-mapped String-with-ivars) is only `eql?` itself.
fn ruby_eql(a: &Loaded, b: &Loaded) -> bool {
    match (a, b) {
        (Loaded::Scalar(x), Loaded::Scalar(y)) => scalar_eql(x, y),
        (Loaded::Seq(x), Loaded::Seq(y)) => {
            x.len() == y.len() && x.iter().zip(y.iter()).all(|(i, j)| ruby_eql(i, j))
        }
        (Loaded::Map(x), Loaded::Map(y)) => {
            x.len() == y.len()
                && x.iter().all(|(k, v)| {
                    y.iter()
                        .any(|(k2, v2)| ruby_eql(k, k2) && ruby_eql(v, v2))
                })
        }
        _ => false,
    }
}

/// `eql?` on the scalar atoms — same class, same value (an Integer is never
/// a Float; `0x10` and `16` are both the Integer 16).
fn scalar_eql(a: &PsychScalar, b: &PsychScalar) -> bool {
    match (a, b) {
        (PsychScalar::Nil, PsychScalar::Nil) => true,
        (PsychScalar::Bool(x), PsychScalar::Bool(y)) => x == y,
        (PsychScalar::Int(x), PsychScalar::Int(y)) => x.value == y.value,
        (PsychScalar::Float(x), PsychScalar::Float(y)) => x == y,
        (PsychScalar::Str(x), PsychScalar::Str(y)) => x == y,
        (PsychScalar::Symbol(x), PsychScalar::Symbol(y)) => x == y,
        _ => false,
    }
}

impl Loaded {
    /// `raw.class` — the `expected a Hash …, got X` subject.
    pub(crate) fn class_name(&self) -> &str {
        match self {
            Loaded::Scalar(s) => s.class_name(),
            Loaded::Seq(_) => "Array",
            Loaded::Map(_) => "Hash",
        }
    }

    /// `raw.is_a?(Hash)`/`is_a?(Array)` truth tables use the variants.
    ///
    /// Ruby `to_s` — collections render their `inspect`.
    pub(crate) fn to_s(&self) -> String {
        match self {
            Loaded::Scalar(s) => s.to_s(),
            _ => self.inspect(),
        }
    }

    /// Ruby `inspect` — `{"a" => 1}`, `[1, "x"]`; an `Opaque` object
    /// renders its `to_s` (no ivar display).
    pub(crate) fn inspect(&self) -> String {
        match self {
            Loaded::Scalar(s) => s.inspect(),
            Loaded::Seq(items) => {
                format!("[{}]", items.iter().map(Loaded::inspect).collect::<Vec<_>>().join(", "))
            }
            Loaded::Map(pairs) => {
                let inner = pairs
                    .iter()
                    .map(|(k, v)| format!("{} => {}", k.inspect(), v.inspect()))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{{{inner}}}")
            }
        }
    }

    /// `raw["version"]`/`row["file"]` — the first `eql?` pair's value.
    pub(crate) fn get<'a>(pairs: &'a [(Loaded, Loaded)], name: &str) -> Option<&'a Loaded> {
        pairs.iter().find(|(k, _)| {
            matches!(k, Loaded::Scalar(PsychScalar::Str(s)) if s == name)
        }).map(|(_, v)| v)
    }

    /// Ruby truthiness — only `nil`/`false` are falsy.
    pub(crate) fn falsy(&self) -> bool {
        matches!(self, Loaded::Scalar(s) if s.falsy())
    }

    /// `row["file"]`/`row["rule"]`'s bucket key — `Str` alone can match a
    /// diagnostic's String path/rule; every other truthy value keys an
    /// inert bucket (`file: 5`, `file: {a: 1}`).
    pub(crate) fn to_key(&self) -> crate::baseline::KeyScalar {
        match self {
            Loaded::Scalar(s) => s.clone().into_key(),
            Loaded::Seq(_) => crate::baseline::KeyScalar::Opaque(self.inspect(), "Array"),
            Loaded::Map(_) => crate::baseline::KeyScalar::Opaque(self.inspect(), "Hash"),
        }
    }
}
