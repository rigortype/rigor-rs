//! A faithful port of the `OptionParser#parse!` subset rigor's CLI depends on
//! (issue #155). Every reference subcommand in `lib/rigor/cli/*_command.rb`
//! builds one `OptionParser` over `opts.on("--name[=ARG]" …)` declarations and
//! calls `parser.parse!(@argv)`; the behaviours reproduced here are exactly the
//! observable ones (stdout, stderr, exit status) of MRI's `optparse.rb`:
//!
//! - `--opt VALUE` and `--opt=VALUE` for required-argument switches;
//!   `--opt[=VALUE]` for optional ones — a separate word is NEVER consumed by
//!   an optional-argument switch;
//! - unambiguous long-option abbreviations (`--basel=x` → `--baseline=x`), an
//!   ambiguous prefix raising `ambiguous option: <token>` — with
//!   `OptionMap#complete`'s prefix-subsumption rule (candidates sorted by name
//!   length; a candidate that is a prefix of the rest resolves);
//! - case-INsensitive completion for the long branch (`complete` runs with
//!   `icase = true` there, so `--FORMAT=json` fires) while the short branch
//!   (`-X…`) completes `X` against the long table case-sensitively;
//! - `_` → `-` normalisation of the long-option NAME (`--no_cache` →
//!   `--no-cache`);
//! - case-SENSITIVE `%w[…]`/`%i[…]` value lists (`CompletingHash`): exact or
//!   unique-prefix values canonicalise (`--match-mode=m` → `message`), a miss
//!   is `invalid argument:`, an ambiguous one `ambiguous argument:`;
//! - `Integer` / `Float` `accept` types, including the reference regexes and
//!   `Integer()`'s radix prefixes (`0x`, `0b`, `0o`, leading-`0` octal) and
//!   `_` separators;
//! - `--` terminates option parsing and is consumed;
//! - `POSIXLY_CORRECT` present in the environment (even empty) switches
//!   `parse!` to `order!`: the first non-option stops the scan and every later
//!   token — options included — is positional;
//! - the default permute mode, where options may follow positionals;
//! - ParseError surfaces: `invalid option:`, `ambiguous option:`,
//!   `invalid argument:`, `missing argument:`, `needless argument:` on stderr
//!   with exit 64, naming the reconstructed argument list the way
//!   `ParseError#set_option` + `message` produce it (`--flag value` for the
//!   separate form, `--flag=value` inline), plus the `Did you mean?` block
//!   from `did_you_mean`'s spell checker (Jaro-Winkler ≥ threshold, then the
//!   gem's Levenshtein filter) for invalid/ambiguous OPTION lookups;
//! - the four officious switches every `OptionParser` carries: `--help`
//!   (prints banner + option summary to stdout, exit 0), `--version`
//!   (`<prog>: version unknown`, exit 1), `--*-completion-bash=WORD` and
//!   `--*-completion-zsh[=NAME]` — and the `--` terminator in the default list.
//!
//! Not ported (nothing rigor declares uses them): short options in tables,
//! `into:`/`accept` classes beyond Integer/Float/completion lists,
//! `~/.options` loading, `on_head`/`on_tail` banner fragments.
//!
//! A command declares a static `&[Switch]` IN `opts.on` ORDER (the order is
//! load-bearing — it feeds completion-candidate iteration, the
//! `Did you mean?` dictionary and `--help` rendering) and folds the returned
//! [`Item`]s into its own options struct.

use std::collections::VecDeque;
use std::process::ExitCode;

// ---------------------------------------------------------------------------
// Switch tables
// ---------------------------------------------------------------------------

/// How a switch treats its argument — `Switch.guess`'s verdict on the `=ARG`
/// decoration (`=ARG` → Required, `=[ARG]` → Optional, none → Flag).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ArgStyle {
    /// `--flag` — a `=VALUE` is a `needless argument` error.
    Flag,
    /// `--flag=VALUE` or `--flag VALUE` — absent is `missing argument`.
    Required,
    /// `--flag[=VALUE]` — a separate `VALUE` word is never consumed.
    Optional,
}

/// The `accept` type of a value-taking switch.
#[derive(Clone, Copy)]
pub enum ValueKind {
    /// No declared type — the value passes through verbatim (Ruby `parse_arg`
    /// with a nil `pattern` returns `[arg]`, so even `""` is accepted).
    Raw,
    /// `accept Integer` — the octal/decimal pattern then `Integer()` (radix
    /// prefixes, `_` separators).
    Int,
    /// `accept Float` — the float pattern then `to_f`.
    Float,
    /// A `%w[..]`/`%i[..]` list → `CompletingHash`: exact or unambiguous
    /// prefix; the delivered value is the canonical entry (a string for us —
    /// symbol/string identity is irrelevant to the port).
    Choice(&'static [&'static str]),
}

/// One `opts.on(...)` declaration.
///
/// `names` is the switch's long-table registrations in order as
/// `(name, negated)`: a literal `--no-cache` is `[("no-cache", false)]` — the
/// `no-` is part of its name — while `--[no-]stats` is
/// `[("stats", false), ("no-stats", true)]` and a `no-stats` hit reports
/// `negated = true`.
pub struct Switch {
    /// The handler key reported in [`Item::Opt`].
    pub key: &'static str,
    /// `(long_name, is_negation)` pairs, declaration order.
    pub names: &'static [(&'static str, bool)],
    pub style: ArgStyle,
    pub kind: ValueKind,
    /// The summary left column: e.g. `"--[no-]stats"`, `"--config"`. For an
    /// argument switch the argument suffix (`"=PATH"`, `"=[LIST]"`) lives in
    /// `arg_desc`. Empty-name switches (`names.is_empty()`) are hidden from
    /// `--help` and completion but still registered (port-only flags use this
    /// so upstream help text stays byte-identical).
    pub ldesc: &'static str,
    /// The argument suffix as declared: `"=PATH"`, `"=[LIST]"`, or `""`.
    pub arg_desc: &'static str,
    /// Description lines — each becomes one right-column summary row.
    pub desc: &'static [&'static str],
    /// Port-only switches set this so `--help` / shell completion render
    /// byte-identically to the upstream table (they remain parseable).
    pub hidden: bool,
}

impl Switch {
    pub const fn new(
        key: &'static str,
        names: &'static [(&'static str, bool)],
        style: ArgStyle,
        kind: ValueKind,
        ldesc: &'static str,
        arg_desc: &'static str,
        desc: &'static [&'static str],
    ) -> Self {
        Switch { key, names, style, kind, ldesc, arg_desc, desc, hidden: false }
    }

    /// A port-only switch hidden from help/completion/dictionary output.
    pub const fn hidden(
        key: &'static str,
        names: &'static [(&'static str, bool)],
        style: ArgStyle,
        kind: ValueKind,
    ) -> Self {
        Switch { key, names, style, kind, ldesc: "", arg_desc: "", desc: &[], hidden: true }
    }
}

// ---------------------------------------------------------------------------
// Parse output
// ---------------------------------------------------------------------------

/// A converted option argument.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Str(String),
    Int(i64),
    Float(f64),
}

impl Value {
    /// The string the upstream block received (canonical entry for `Choice`,
    /// the raw token otherwise).
    pub fn as_str(&self) -> &str {
        match self {
            Value::Str(s) => s,
            _ => "",
        }
    }
    pub fn as_int(&self) -> i64 {
        match self {
            Value::Int(i) => *i,
            _ => 0,
        }
    }
    pub fn as_float(&self) -> f64 {
        match self {
            Value::Float(f) => *f,
            _ => 0.0,
        }
    }
}

/// One classified argv element, in scan order (positionals land after every
/// fired option — `permute` semantics).
#[derive(Debug, Clone)]
pub enum Item {
    /// A switch fired. `negated` is set for the `no-` side of a `--[no-]x`
    /// declaration. `value` is `Some` for `Required` (always) and `Optional`
    /// (only on the `=` form) switches.
    ///
    /// `negated` is part of the faithful parse surface — every `--[no-]x`
    /// consumer in this port (`--[no-]stats`, `--[no-]color`, `--[no-]bat`)
    /// accepts both sides as inert, so no fold reads it yet.
    #[allow(dead_code)]
    Opt { key: &'static str, negated: bool, value: Option<Value> },
    /// A non-option argument — or everything after `--` / after the first
    /// non-option under `POSIXLY_CORRECT`.
    Positional(String),
}

/// The parse outcome: either the classified argv, or an immediate exit — a
/// `ParseError` (stderr + 64) or an officious switch (`--help`, `--version`,
/// `--*-completion-*`) that upstream answers by printing and `exit`ing.
pub enum Parsed {
    Items(Vec<Item>),
    /// Print `stdout` to stdout and `stderr` to stderr, exit `code` — the
    /// `puts`/`warn`/`abort` result the upstream command never returns from.
    Exit { stdout: String, stderr: String, code: u8 },
}

impl Parsed {
    /// Consume into the item list, or print + hand back the exit code.
    pub fn items_or_exit(self) -> Result<Vec<Item>, ExitCode> {
        match self {
            Parsed::Items(items) => Ok(items),
            Parsed::Exit { stdout, stderr, code } => {
                if !stdout.is_empty() {
                    print!("{stdout}");
                }
                if !stderr.is_empty() {
                    eprint!("{stderr}");
                }
                Err(ExitCode::from(code))
            }
        }
    }

    /// `items_or_exit` variant for commands whose `parse_options` rescues
    /// `OptionParser::ParseError` and appends the USAGE text (lsp/mcp).
    pub fn items_or_exit_usage(self, usage: &'static str) -> Result<Vec<Item>, ExitCode> {
        match self {
            Parsed::Items(items) => Ok(items),
            Parsed::Exit { stdout, stderr, code } => {
                if !stdout.is_empty() {
                    print!("{stdout}");
                }
                if !stderr.is_empty() {
                    eprint!("{stderr}");
                }
                if code == 64 {
                    eprintln!("{usage}");
                }
                Err(ExitCode::from(code))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The parser
// ---------------------------------------------------------------------------

/// The officious switches `OptionParser#add_officious` registers in the base
/// list — searched after the command's own table.
const OFFICIOUS: &[&str] = &["help", "*-completion-bash", "*-completion-zsh", "version"];

/// `program_name` — `File.basename($0)` without an extension.
fn program_name() -> String {
    std::env::args()
        .next()
        .and_then(|a| {
            std::path::Path::new(&a).file_stem().map(|s| s.to_string_lossy().into_owned())
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "rigor".to_string())
}

pub struct OptParser {
    /// `opts.banner` — the first line of `--help` output.
    banner: &'static str,
    /// The command's switch table in `opts.on` order.
    switches: &'static [Switch],
}

/// What a long name resolved to.
#[derive(Clone, Copy, PartialEq)]
enum Resolved {
    /// A command switch: index into `switches` + the negation flag.
    Switch(usize, bool),
    /// `long[""]` — the `--` terminator.
    Terminator,
    Help,
    Version,
    CompletionBash,
    CompletionZsh,
}

enum Lookup {
    Found(Resolved),
    Invalid,
    Ambiguous,
}

impl OptParser {
    pub const fn new(banner: &'static str, switches: &'static [Switch]) -> Self {
        OptParser { banner, switches }
    }

    // ---- name tables -------------------------------------------------

    fn officious(name: &str) -> Option<Resolved> {
        match name {
            "help" => Some(Resolved::Help),
            "version" => Some(Resolved::Version),
            "*-completion-bash" => Some(Resolved::CompletionBash),
            "*-completion-zsh" => Some(Resolved::CompletionZsh),
            _ => None,
        }
    }

    /// `search(:long, key)`: exact match — command table, then officious,
    /// then the default list's `""` terminator.
    fn exact(&self, name: &str) -> Option<Resolved> {
        for (i, sw) in self.switches.iter().enumerate() {
            for &(n, neg) in sw.names {
                if n == name {
                    return Some(Resolved::Switch(i, neg));
                }
            }
        }
        if let Some(r) = Self::officious(name) {
            return Some(r);
        }
        if name.is_empty() {
            return Some(Resolved::Terminator);
        }
        None
    }

    /// `complete(:long, key, icase)` — per the private `OptionParser#complete`:
    /// exact first, then per-list completion in stack order (command table →
    /// officious; the default list's only key `""` can never prefix-match a
    /// non-empty input). Within a list, `OptionMap#complete` resolves a
    /// unique or prefix-subsumed candidate set and throws on a real
    /// ambiguity; zero candidates falls through to the next list.
    fn complete(&self, name: &str, icase: bool) -> Lookup {
        if let Some(r) = self.exact(name) {
            return Lookup::Found(r);
        }
        let mut cands: Vec<(&str, Resolved)> = Vec::new();
        for (i, sw) in self.switches.iter().enumerate() {
            for &(n, neg) in sw.names {
                if completion_match(name, n, icase) {
                    cands.push((n, Resolved::Switch(i, neg)));
                }
            }
        }
        if let Some(r) = resolve_candidates(&cands) {
            return r;
        }
        cands.clear();
        for &n in OFFICIOUS {
            if completion_match(name, n, icase) {
                cands.push((n, Self::officious(n).unwrap()));
            }
        }
        if let Some(r) = resolve_candidates(&cands) {
            return r;
        }
        Lookup::Invalid
    }

    /// The `Did you mean?` dictionary — every long key: command names first,
    /// then the officious names, then the default list's `""`.
    fn dictionary(&self) -> Vec<String> {
        let mut d: Vec<String> = Vec::new();
        for sw in self.switches {
            if sw.hidden {
                continue;
            }
            for &(n, _) in sw.names {
                d.push(n.to_string());
            }
        }
        d.extend(OFFICIOUS.iter().map(|s| s.to_string()));
        d.push(String::new());
        d
    }

    // ---- parse ------------------------------------------------------

    /// `parser.parse!(argv)` — `order!` when `POSIXLY_CORRECT` is present
    /// (even empty), `permute!` otherwise.
    pub fn parse(&self, argv: &[String]) -> Parsed {
        let posix = std::env::var_os("POSIXLY_CORRECT").is_some();
        let mut rest: VecDeque<String> = argv.iter().cloned().collect();
        let mut items: Vec<Item> = Vec::new();
        let mut nonopts: Vec<String> = Vec::new();

        while let Some(tok) = rest.pop_front() {
            if tok.starts_with("--") {
                // Long option: /\A--([^=]*)(?:=(.*))?/m
                let (name_raw, eq) = match tok[2..].find('=') {
                    Some(p) => (&tok[2..2 + p], Some(tok[2 + p + 1..].to_string())),
                    None => (&tok[2..], None),
                };
                // `opt.tr!('_', '-')` — underscores in the NAME become dashes.
                let name = name_raw.replace('_', "-");
                let resolved = match self.complete(&name, true) {
                    Lookup::Found(r) => r,
                    Lookup::Invalid => {
                        return self.err(&format!("invalid option: {tok}"), Some(&name))
                    }
                    Lookup::Ambiguous => {
                        return self.err(&format!("ambiguous option: {tok}"), Some(&name))
                    }
                };
                match resolved {
                    Resolved::Terminator => {
                        // `--` (a `=x` tail is a NoArgument's needless-argument
                        // error first).
                        if eq.is_some() {
                            return self.err(&format!("needless argument: {tok}"), None);
                        }
                        break;
                    }
                    Resolved::Help => {
                        if eq.is_some() {
                            return self.err(&format!("needless argument: {tok}"), None);
                        }
                        return Parsed::Exit {
                            stdout: self.help_text(),
                            stderr: String::new(),
                            code: 0,
                        };
                    }
                    Resolved::Version => {
                        // OptionalArgument — a `=PKG` value names the package.
                        let msg = match eq {
                            Some(pkg) => {
                                format!("{}: no version found in package {pkg}", program_name())
                            }
                            None => format!("{}: version unknown", program_name()),
                        };
                        return Parsed::Exit {
                            stdout: String::new(),
                            stderr: msg + "\n",
                            code: 1,
                        };
                    }
                    Resolved::CompletionBash => {
                        // RequiredArgument — `=WORD` or the next token.
                        let word = match eq {
                            Some(w) => w,
                            None => match rest.pop_front() {
                                Some(w) => w,
                                None => {
                                    return self
                                        .err(&format!("missing argument: {tok}"), None)
                                }
                            },
                        };
                        let mut out = String::new();
                        for c in self.candidates(&word) {
                            out.push_str(&c);
                            out.push('\n');
                        }
                        if out.is_empty() {
                            // `puts []` emits a lone newline.
                            out.push('\n');
                        }
                        return Parsed::Exit { stdout: out, stderr: String::new(), code: 0 };
                    }
                    Resolved::CompletionZsh => {
                        let out = self.compsys_text(eq.as_deref());
                        return Parsed::Exit { stdout: out, stderr: String::new(), code: 0 };
                    }
                    Resolved::Switch(idx, negated) => {
                        let sw = &self.switches[idx];
                        match sw.style {
                            ArgStyle::Flag => {
                                if eq.is_some() {
                                    return self
                                        .err(&format!("needless argument: {tok}"), None);
                                }
                                items.push(Item::Opt { key: sw.key, negated, value: None });
                            }
                            ArgStyle::Required => {
                                let (v, via_eq) = match eq {
                                    Some(v) => (v, true),
                                    None => match rest.pop_front() {
                                        Some(v) => (v, false),
                                        None => {
                                            return self.err(
                                                &format!("missing argument: {tok}"),
                                                None,
                                            )
                                        }
                                    },
                                };
                                match convert(sw.kind, &v) {
                                    Ok(val) => items.push(Item::Opt {
                                        key: sw.key,
                                        negated,
                                        value: Some(val),
                                    }),
                                    Err(e) => {
                                        return self.err(&value_error(e, &tok, via_eq, &v), None)
                                    }
                                }
                            }
                            ArgStyle::Optional => match eq {
                                Some(v) => match convert(sw.kind, &v) {
                                    Ok(val) => items.push(Item::Opt {
                                        key: sw.key,
                                        negated,
                                        value: Some(val),
                                    }),
                                    Err(e) => {
                                        return self.err(&value_error(e, &tok, true, &v), None)
                                    }
                                },
                                None => items.push(Item::Opt {
                                    key: sw.key,
                                    negated,
                                    value: None,
                                }),
                            },
                        }
                    }
                }
            } else if tok.starts_with('-') && tok.len() > 1 {
                // Short option: /\A-(.)((=).*|.+)?/m — no command declares
                // short options, so `search`/`complete(:short)` always miss
                // and the fall-through `complete(:long, opt)` (icase FALSE)
                // resolves the first char against the long table. On a hit
                // the switch's parse runs over the token remainder (`val`,
                // '='-included) — its leftover is re-queued as a fresh `-x`
                // token — and on `-x` alone `eq ||= !rest` makes the error
                // block raise (so `-h` is needless-argument-fatal under `=`,
                // and a bare `-f` fires `--format` consuming the next token).
                let opt = tok[1..].chars().next().unwrap().to_string();
                let rest_str = &tok[1 + opt.len()..];
                let has_rest = !rest_str.is_empty();
                let eq = !has_rest || rest_str.starts_with('=');
                let resolved = match self.complete(&opt, false) {
                    Lookup::Found(r) => r,
                    Lookup::Invalid => {
                        return self.err(&format!("invalid option: {tok}"), Some(&opt))
                    }
                    Lookup::Ambiguous => {
                        return self.err(&format!("ambiguous option: {tok}"), Some(&opt))
                    }
                };
                let (key, negated, style, kind) = match resolved {
                    Resolved::Switch(i, n) => {
                        let sw = &self.switches[i];
                        (sw.key, n, sw.style, sw.kind)
                    }
                    Resolved::Help => ("@help", false, ArgStyle::Flag, ValueKind::Raw),
                    Resolved::Version => {
                        ("@version", false, ArgStyle::Optional, ValueKind::Raw)
                    }
                    Resolved::CompletionBash => {
                        ("@comp-bash", false, ArgStyle::Required, ValueKind::Raw)
                    }
                    Resolved::CompletionZsh => {
                        ("@comp-zsh", false, ArgStyle::Optional, ValueKind::Raw)
                    }
                    // `""` can't resolve from a one-char long name.
                    Resolved::Terminator => unreachable!(),
                };
                // `sw.parse(val, argv)` where `val` is the raw remainder
                // (INCLUDING the '=' for `-f=x` — the CompletingHash lookup
                // then misses, producing `invalid argument:`).
                let val: Option<&str> = if has_rest { Some(rest_str) } else { None };
                let mut leftover: Option<String> = None;
                let fired: Option<Option<Value>>;
                match style {
                    ArgStyle::Flag => {
                        // NoArgument: `yield(NeedlessArgument, arg) if arg` —
                        // the block raises iff `eq`; otherwise the argument is
                        // a parse leftover re-queued below.
                        if let Some(v) = val {
                            if eq {
                                return self
                                    .err(&format!("needless argument: {tok}"), None);
                            }
                            leftover = Some(v.to_string());
                        }
                        fired = Some(None);
                    }
                    ArgStyle::Required => {
                        // `unless arg` → shift argv; empty argv →
                        // MissingArgument (`set_option(arg, len>2)` — the
                        // one-token message either way).
                        let v: String = match val {
                            Some(v) => v.to_string(),
                            None => match rest.pop_front() {
                                Some(v) => v,
                                None => {
                                    return self
                                        .err(&format!("missing argument: {tok}"), None)
                                }
                            },
                        };
                        match convert(kind, &v) {
                            Ok(cv) => fired = Some(Some(cv)),
                            Err(e) => {
                                return self.err(
                                    &short_value_error(e, &tok, val.is_none(), &v),
                                    None,
                                )
                            }
                        }
                    }
                    ArgStyle::Optional => {
                        match val {
                            Some(v) => match convert(kind, v) {
                                Ok(cv) => fired = Some(Some(cv)),
                                Err(e) => {
                                    return self
                                        .err(&short_value_error(e, &tok, false, v), None)
                                }
                            },
                            None => fired = Some(None),
                        }
                    }
                }
                // Fire the switch — officious ones exit like the long branch.
                match key {
                    "@help" => {
                        return Parsed::Exit {
                            stdout: self.help_text(),
                            stderr: String::new(),
                            code: 0,
                        }
                    }
                    "@version" => {
                        let msg = match &fired {
                            Some(Some(Value::Str(pkg))) => format!(
                                "{}: no version found in package {pkg}",
                                program_name()
                            ),
                            _ => format!("{}: version unknown", program_name()),
                        };
                        return Parsed::Exit {
                            stdout: String::new(),
                            stderr: msg + "\n",
                            code: 1,
                        };
                    }
                    "@comp-bash" => {
                        let word = match &fired {
                            Some(Some(Value::Str(s))) => s.clone(),
                            _ => String::new(),
                        };
                        let mut out = String::new();
                        for c in self.candidates(&word) {
                            out.push_str(&c);
                            out.push('\n');
                        }
                        if out.is_empty() {
                            out.push('\n');
                        }
                        return Parsed::Exit {
                            stdout: out,
                            stderr: String::new(),
                            code: 0,
                        };
                    }
                    "@comp-zsh" => {
                        let name = match &fired {
                            Some(Some(Value::Str(s))) => Some(s.as_str()),
                            _ => None,
                        };
                        let out = self.compsys_text(name);
                        return Parsed::Exit {
                            stdout: out,
                            stderr: String::new(),
                            code: 0,
                        };
                    }
                    _ => {
                        if let Some(v) = fired {
                            items.push(Item::Opt { key, negated, value: v });
                        }
                    }
                }
                // `argv.unshift(opt) if opt and (!rest or (opt =
                // opt.sub(/\A-*/, '-')) != '-')` — the leftover re-queues as
                // one `-`-prefixed token (or bare, when there was no rest).
                if let Some(lo) = leftover {
                    if !has_rest {
                        rest.push_front(lo);
                    } else {
                        let pushed = format!("-{}", lo.trim_start_matches('-'));
                        if pushed != "-" {
                            rest.push_front(pushed);
                        }
                    }
                }
            } else {
                // Non-option: `order!` (POSIXLY_CORRECT) stops here, leaving
                // the token and the whole tail as positionals; `permute!`
                // collects it and keeps scanning.
                if posix {
                    rest.push_front(tok);
                    break;
                }
                nonopts.push(tok);
            }
        }

        for s in nonopts.into_iter().chain(rest.into_iter()) {
            items.push(Item::Positional(s));
        }
        Parsed::Items(items)
    }

    /// A `ParseError` surface: `reason: {args}` on stderr + the
    /// `did_you_mean` suggestion block for invalid/ambiguous OPTION lookups.
    fn err(&self, msg: &str, suggest_for: Option<&str>) -> Parsed {
        let mut stderr = format!("{msg}\n");
        if let Some(opt) = suggest_for {
            let dict = self.dictionary();
            let corrections = spell_correct(&dict, opt);
            if !corrections.is_empty() {
                stderr.push_str(&format!("Did you mean?  {}", corrections.join("\n               ")));
                stderr.push('\n');
            }
        }
        Parsed::Exit { stdout: String::new(), stderr, code: 64 }
    }

    // ---- rendering --------------------------------------------------

    /// `puts parser` — banner + switch summary, the `Switch#summarize` /
    /// `List#summarize` layout: `@summary_indent` (4 spaces) wraps each
    /// emitted line; the option column itself starts with another 4 spaces
    /// and pads to width 32 before `' ' + desc`. Hidden (port-only) switches
    /// render nothing — upstream help text stays byte-identical.
    fn help_text(&self) -> String {
        const INDENT: &str = "    ";
        const WIDTH: usize = 32;
        let mut out = String::new();
        out.push_str(self.banner.strip_suffix('\n').unwrap_or(self.banner));
        out.push('\n');
        for sw in self.switches {
            if sw.hidden || sw.names.is_empty() {
                continue;
            }
            // left = [sopts.join(', ')] = [""]; each lopt lands on the last
            // element (`l < max or sopts.empty?` — sopts is empty, so it
            // never splits), prefixed ' ' * 4; then `left[0] << arg`.
            let mut lefts: Vec<String> = vec![format!("    {}{}", sw.ldesc, sw.arg_desc)];
            let mut right: Vec<&str> = sw.desc.to_vec();
            // `while mlen > width and l = left.shift`
            loop {
                let mlen = lefts.iter().map(|l| l.len()).max().unwrap_or(0);
                if mlen <= WIDTH || lefts.is_empty() {
                    break;
                }
                let l = lefts.remove(0);
                if l.len() < WIDTH {
                    if let Some(r) = right.first().copied() {
                        if !r.is_empty() {
                            out.push_str(&format!("{INDENT}{l:<WIDTH$} {r}\n"));
                            right.remove(0);
                            continue;
                        }
                    }
                }
                out.push_str(&format!("{INDENT}{l}\n"));
            }
            // `while begin l = left.shift; r = right.shift; l or r end`
            loop {
                let l = if lefts.is_empty() { None } else { Some(lefts.remove(0)) };
                let r = if right.is_empty() { None } else { Some(right.remove(0)) };
                if l.is_none() && r.is_none() {
                    break;
                }
                let (l, r) = (l.unwrap_or_default(), r.unwrap_or_default());
                if !r.is_empty() {
                    out.push_str(&format!("{INDENT}{l:<WIDTH$} {r}\n"));
                } else {
                    out.push_str(&format!("{INDENT}{l}\n"));
                }
            }
        }
        out
    }

    /// `parser.candidate(word)` for `--*-completion-bash=WORD`.
    fn candidates(&self, word: &str) -> Vec<String> {
        // `case word`: "-" → long (+short, empty table); "--..." → long with
        // the value split out; "-..." → short only (empty table → nothing).
        let (w, arg) = if word == "-" {
            (word, None)
        } else if word.starts_with("--") {
            match word.split_once('=') {
                Some((h, a)) => (h, Some(a)),
                None => (word, None),
            }
        } else {
            (word, None)
        };
        let long = word == "-" || word.starts_with("--");
        let mut list: Vec<String> = Vec::new();
        for sw in self.switches {
            if sw.hidden {
                continue;
            }
            let mut opts: Vec<String> = Vec::new();
            if long && completion_match(w, sw.ldesc, true) {
                opts.push(sw.ldesc.to_string());
            }
            if sw.arg_desc.starts_with('=') {
                for o in opts.iter_mut() {
                    o.push('=');
                }
                // `--opt=WORD` on a `CompletingHash` switch replaces the
                // candidates with the completed VALUES (canonical keys).
                if let (Some(a), ValueKind::Choice(choices)) = (arg, sw.kind) {
                    opts = choices
                        .iter()
                        .copied()
                        .filter(|c| completion_match(a, c, false))
                        .map(str::to_string)
                        .collect();
                }
            }
            list.extend(opts);
        }
        list
    }

    /// `parser.compsys($stdout, name)` for `--*-completion-zsh[=NAME]`.
    fn compsys_text(&self, name: Option<&str>) -> String {
        let default_name;
        let name = match name {
            Some(n) => n,
            None => {
                default_name = program_name();
                default_name.as_str()
            }
        };
        let mut out = format!("#compdef {name}\n");
        out.push_str("\ntypeset -A opt_args\nlocal context state line\n\n_arguments -s -S \\\n");
        for sw in self.switches {
            if sw.hidden || sw.names.is_empty() {
                continue;
            }
            let d = sw.desc.concat();
            let mut esc = String::with_capacity(d.len());
            for c in d.chars() {
                match c {
                    '\\' | '"' | '[' | ']' => {
                        esc.push('\\');
                        esc.push(c);
                    }
                    _ => esc.push(c),
                }
            }
            if let Some(inner) = sw.ldesc.strip_prefix("--[no-]") {
                out.push_str(&format!("  \"--{inner}[{esc}]\" \\\n"));
                out.push_str(&format!("  \"--no-{inner}[{esc}]\" \\\n"));
            } else {
                out.push_str(&format!("  \"{}[{esc}]\" \\\n", sw.ldesc));
            }
        }
        out.push_str("  '*:file:_files' && return 0\n");
        out
    }
}

// ---------------------------------------------------------------------------
// Completion matching — `Completion.regexp(key, icase)`
// ---------------------------------------------------------------------------

fn is_word(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// The `\A` + `Regexp.quote(key).gsub(/\w+\b/, '\&\w*')` pattern: each maximal
/// word-run in `key` must match a prefix of a word-run in `cand`, every other
/// char is literal. Case-insensitive when `icase` (the long branch).
fn completion_match(key: &str, cand: &str, icase: bool) -> bool {
    let key: Vec<char> = key.chars().collect();
    let cand: Vec<char> = cand.chars().collect();
    let mut kp = 0;
    let mut cp = 0;
    let eq =
        |a: char, b: char| if icase { a.eq_ignore_ascii_case(&b) } else { a == b };
    while kp < key.len() {
        if is_word(key[kp]) {
            let start = kp;
            while kp < key.len() && is_word(key[kp]) {
                kp += 1;
            }
            let run = &key[start..kp];
            if cp + run.len() > cand.len() {
                return false;
            }
            for (i, &ch) in run.iter().enumerate() {
                if !eq(cand[cp + i], ch) {
                    return false;
                }
            }
            cp += run.len();
            while cp < cand.len() && is_word(cand[cp]) {
                cp += 1;
            }
        } else {
            if cp >= cand.len() || !eq(cand[cp], key[kp]) {
                return false;
            }
            kp += 1;
            cp += 1;
        }
    }
    true
}

/// `OptionMap#complete`'s ambiguity resolution over one list's
/// pattern-matched candidates: sort by name length (stable), a unique or
/// prefix-subsumed set resolves, anything else is ambiguous.
fn resolve_candidates(cands: &[(&str, Resolved)]) -> Option<Lookup> {
    if cands.is_empty() {
        return None;
    }
    let mut sorted: Vec<(&str, Resolved)> = cands.to_vec();
    sorted.sort_by_key(|(n, _)| n.chars().count()); // sort_by kn.size — stable
    let (mut canon_name, mut canon) = (sorted[0].0, sorted[0].1);
    for &(kn, v) in &sorted[1..] {
        // `next if sw == v` — the same switch reached under a second name
        // never self-ambiguates.
        if let (Resolved::Switch(a, na), Resolved::Switch(b, nb)) = (canon, v) {
            if a == b && na == nb {
                continue;
            }
        }
        if canon_name.starts_with(kn) {
            // `cn.rindex(kn, 0)` — kn is a strict prefix of cn.
            canon_name = kn;
            canon = v;
            continue;
        }
        if kn.starts_with(canon_name) {
            continue;
        }
        return Some(Lookup::Ambiguous);
    }
    Some(Lookup::Found(canon))
}

// ---------------------------------------------------------------------------
// Value conversion
// ---------------------------------------------------------------------------

/// `invalid` vs `ambiguous` — the two value-error reasons.
pub enum ValueError {
    Invalid,
    Ambiguous,
}

/// Convert a switch value per its `accept` type.
fn convert(kind: ValueKind, v: &str) -> Result<Value, ValueError> {
    match kind {
        ValueKind::Raw => Ok(Value::Str(v.to_string())),
        ValueKind::Int => {
            if !int_pattern(v) {
                return Err(ValueError::Invalid);
            }
            Ok(Value::Int(parse_ruby_int(v).unwrap_or(i64::MAX)))
        }
        ValueKind::Float => {
            if !float_pattern(v) {
                return Err(ValueError::Invalid);
            }
            let cleaned: String = v.chars().filter(|&c| c != '_').collect();
            Ok(Value::Float(cleaned.parse::<f64>().unwrap_or(0.0)))
        }
        ValueKind::Choice(list) => {
            // `CompletingHash#match`: exact fetch, else unique-prefix
            // completion (same subsumption rule as option names).
            if list.contains(&v) {
                return Ok(Value::Str(v.to_string()));
            }
            let mut cands: Vec<&'static str> =
                list.iter().copied().filter(|c| completion_match(v, c, false)).collect();
            cands.sort_by_key(|c| c.len());
            match cands.len() {
                0 => Err(ValueError::Invalid),
                1 => Ok(Value::Str(cands[0].to_string())),
                _ => {
                    let mut canon = cands[0];
                    for &c in &cands[1..] {
                        if canon.starts_with(c) {
                            canon = c;
                        } else if !c.starts_with(canon) {
                            return Err(ValueError::Ambiguous);
                        }
                    }
                    Ok(Value::Str(canon.to_string()))
                }
            }
        }
    }
}

/// The `ParseError#message` for a value error: `set_option(arg, rest)` puts
/// the whole token back for the `=` form and unshifts the option token for
/// the separate form, so the printed argument list is `--flag=v` vs
/// `--flag v` respectively.
fn value_error(kind: ValueError, token: &str, via_eq: bool, value: &str) -> String {
    let reason = match kind {
        ValueError::Invalid => "invalid argument",
        ValueError::Ambiguous => "ambiguous argument",
    };
    if via_eq {
        format!("{reason}: {token}")
    } else {
        format!("{reason}: {token} {value}")
    }
}

/// Same for the short branch, where `set_option(arg, arg.length > 2)`:
/// a multi-char token (`-fx`, `-f=x`, `-top`) shows whole; a bare `-f` whose
/// value came from argv renders `-f <value>`.
fn short_value_error(kind: ValueError, token: &str, shifted: bool, value: &str) -> String {
    let reason = match kind {
        ValueError::Invalid => "invalid argument",
        ValueError::Ambiguous => "ambiguous argument",
    };
    if shifted {
        format!("{reason}: {token} {value}")
    } else {
        format!("{reason}: {token}")
    }
}

// ---------------------------------------------------------------------------
// Integer / Float patterns — the `accept` regexes, ported
// ---------------------------------------------------------------------------

/// `\A[-+]?(?:0(?:[0-7]+(?:_[0-7]+)*|b[01]+(?:_[01]+)*|x[\da-f]+(?:_[\da-f]+)*)?|\d+(?:_\d+)*)\z`i
fn int_pattern(s: &str) -> bool {
    let body = s.strip_prefix(['-', '+']).unwrap_or(s);
    if body.is_empty() {
        return false;
    }
    // `\d+(?:_\d+)*` — digit groups separated by single underscores.
    let group_ok = |t: &str, ok: fn(char) -> bool| -> bool {
        if t.is_empty() || !ok(t.chars().next().unwrap()) {
            return false;
        }
        let mut prev_us = false;
        for c in t.chars() {
            if c == '_' {
                if prev_us {
                    return false;
                }
                prev_us = true;
            } else if ok(c) {
                prev_us = false;
            } else {
                return false;
            }
        }
        !prev_us
    };
    let lower = body.to_ascii_lowercase();
    if let Some(rest) = lower.strip_prefix('0') {
        if rest.is_empty() {
            return true; // bare "0"
        }
        if let Some(b) = rest.strip_prefix('b') {
            return group_ok(b, |c| matches!(c, '0' | '1'));
        }
        if let Some(x) = rest.strip_prefix('x') {
            return group_ok(x, |c| c.is_ascii_hexdigit());
        }
        // `0(?:[0-7]+(?:_[0-7]+)*)?` — a leading `_` fails (the inner group is
        // optional but a bare `_` is not a `[0-7]` start).
        return group_ok(rest, |c| ('0'..='7').contains(&c));
    }
    group_ok(body, |c| c.is_ascii_digit())
}

/// `Integer(s)`: radix-prefixed (`0x`/`0b`/`0o`/leading-`0` octal) else
/// decimal; sign first; `_` stripped. `None` on overflow — clamped by the
/// caller (upstream bignums never fail).
fn parse_ruby_int(s: &str) -> Option<i64> {
    let (neg, body) = match s.strip_prefix('-') {
        Some(b) => (true, b),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    let body: String = body.chars().filter(|&c| c != '_').collect();
    let (radix, digits) = if let Some(h) =
        body.strip_prefix("0x").or_else(|| body.strip_prefix("0X"))
    {
        (16, h)
    } else if let Some(b) = body.strip_prefix("0b").or_else(|| body.strip_prefix("0B")) {
        (2, b)
    } else if let Some(o) = body.strip_prefix("0o").or_else(|| body.strip_prefix("0O")) {
        (8, o)
    } else if body.len() > 1 && body.starts_with('0') {
        (8, &body[1..])
    } else {
        (10, body.as_str())
    };
    i64::from_str_radix(digits, radix).ok().map(|v| if neg { -v } else { v })
}

/// `\A[-+]?float\z`i — float = `(?:dec(?=(.)?)(?:\.(?:dec)?)?|\.dec)(?:[eE][-+]?dec)?`,
/// dec = `\d+(?:_\d+)*`.
fn float_pattern(s: &str) -> bool {
    let s = s.strip_prefix(['-', '+']).unwrap_or(s);
    let dec = |t: &str| -> bool {
        if t.is_empty() || !t.chars().next().unwrap().is_ascii_digit() {
            return false;
        }
        let mut prev_us = false;
        for c in t.chars() {
            if c == '_' {
                if prev_us {
                    return false;
                }
                prev_us = true;
            } else if c.is_ascii_digit() {
                prev_us = false;
            } else {
                return false;
            }
        }
        !prev_us
    };
    let (mantissa, exp) = match s.find(['e', 'E']) {
        Some(p) => (&s[..p], Some(&s[p + 1..])),
        None => (s, None),
    };
    let mantissa_ok = match mantissa.find('.') {
        Some(dot) => {
            let (a, b) = (&mantissa[..dot], &mantissa[dot + 1..]);
            if a.is_empty() {
                dec(b) // ".5"
            } else {
                dec(a) && (b.is_empty() || dec(b)) // "5." / "5.5"
            }
        }
        None => dec(mantissa),
    };
    if !mantissa_ok {
        return false;
    }
    match exp {
        None => true,
        Some(e) => dec(e.strip_prefix(['-', '+']).unwrap_or(e)),
    }
}

// ---------------------------------------------------------------------------
// did_you_mean — Jaro-Winkler + the gem's Levenshtein, verbatim ports
// ---------------------------------------------------------------------------

fn jaro(s1: &[u32], s2: &[u32]) -> f64 {
    let (s1, s2) = if s1.len() > s2.len() { (s2, s1) } else { (s1, s2) };
    let (l1, l2) = (s1.len(), s2.len());
    if l1 == 0 || l2 == 0 || l1 > 120 || l2 > 120 {
        return 0.0; // flags are u128 — cap keeps the bit ops in range
    }
    let mut m = 0.0f64;
    let mut flags1 = 0u128;
    let mut flags2 = 0u128;
    let range = if l2 > 3 { l2 / 2 - 1 } else { 0 };
    for i in 0..l1 {
        let last = i + range;
        let mut j = if i >= range { i - range } else { 0 };
        while j <= last && j < l2 {
            if flags2 & (1 << j) == 0 && s1[i] == s2[j] {
                flags2 |= 1 << j;
                flags1 |= 1 << i;
                m += 1.0;
                break;
            }
            j += 1;
        }
    }
    let mut t = 0.0f64;
    let mut k = 0usize;
    for i in 0..l1 {
        if flags1 & (1 << i) == 0 {
            continue;
        }
        let mut j = k;
        let mut index = k;
        while j < l2 {
            index = j;
            if flags2 & (1 << j) != 0 {
                break;
            }
            j += 1;
        }
        // `k = break(j + 1)` — the loop always breaks before l2 when a
        // flags1 bit is set (each has a flagged partner).
        k = if j < l2 { j + 1 } else { l2 };
        if index < l2 && s1[i] != s2[index] {
            t += 1.0;
        }
    }
    let t = (t / 2.0).floor();
    if m == 0.0 {
        0.0
    } else {
        (m / l1 as f64 + m / l2 as f64 + (m - t) / m) / 3.0
    }
}

fn jaro_winkler(a: &str, b: &str) -> f64 {
    let s1: Vec<u32> = a.chars().map(|c| c as u32).collect();
    let s2: Vec<u32> = b.chars().map(|c| c as u32).collect();
    let d = jaro(&s1, &s2);
    if d > 0.7 {
        let mut prefix = 0usize;
        for &c in &s1 {
            if prefix < s2.len() && c == s2[prefix] && prefix < 4 {
                prefix += 1;
            } else {
                break;
            }
        }
        d + prefix as f64 * 0.1 * (1.0 - d)
    } else {
        d
    }
}

/// The did_you_mean gem's Levenshtein (the `text`-gem port — `d[j] = i`
/// inside the scan and the mutating `i` are its own quirks, kept verbatim).
fn dym_levenshtein(a: &str, b: &str) -> usize {
    let s1: Vec<u32> = a.chars().map(|c| c as u32).collect();
    let s2: Vec<u32> = b.chars().map(|c| c as u32).collect();
    let n = s1.len();
    let m = s2.len();
    if n == 0 {
        return m;
    }
    if m == 0 {
        return n;
    }
    let mut d: Vec<usize> = (0..=m).collect();
    let mut x = 0usize;
    for i1 in 0..n {
        let mut i = i1 + 1;
        let mut j = 0usize;
        while j < m {
            let cost = usize::from(s1[i1] != s2[j]);
            let a_ = d[j + 1] + 1;
            let b_ = i + 1;
            let c_ = d[j] + cost;
            x = if a_ < b_ && a_ < c_ {
                a_
            } else if b_ < c_ {
                b_
            } else {
                c_
            };
            d[j] = i;
            i = x;
            j += 1;
        }
        d[m] = x;
    }
    x
}

/// `DidYouMean::SpellChecker#correct(input)` intersected with the dictionary
/// in dictionary order (the `all_candidates & correct` in
/// `OptionParser#additional_message`).
fn spell_correct(dict: &[String], input: &str) -> Vec<String> {
    let norm = |s: &str| s.to_lowercase().replace('@', "");
    let ni = norm(input);
    let threshold = if ni.chars().count() > 3 { 0.834 } else { 0.77 };
    let mut words: Vec<&String> = dict
        .iter()
        .filter(|w| jaro_winkler(&norm(w), &ni) >= threshold)
        .collect();
    words.retain(|w| input != w.as_str());
    let mut scored: Vec<(&String, f64)> =
        words.iter().map(|w| (*w, jaro_winkler(w, &ni))).collect();
    scored.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
    scored.reverse();
    let words: Vec<&String> = scored.into_iter().map(|(w, _)| w).collect();
    let lev_thr = (ni.chars().count() as f64 * 0.25).ceil() as usize;
    let mut corrections: Vec<String> = words
        .iter()
        .filter(|c| dym_levenshtein(&norm(c), &ni) <= lev_thr)
        .map(|w| (*w).clone())
        .collect();
    if corrections.is_empty() {
        for w in &words {
            let nw = norm(w);
            let length = ni.chars().count().min(nw.chars().count());
            if dym_levenshtein(&nw, &ni) < length {
                corrections.push((*w).clone());
                break; // `.first(1)`
            }
        }
    }
    let mut out: Vec<String> = Vec::new();
    for cand in dict {
        if corrections.iter().any(|c| c == cand) && !out.iter().any(|c| c == cand) {
            out.push(cand.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const TBL: &[Switch] = &[
        Switch::new(
            "config",
            &[("config", false)],
            ArgStyle::Required,
            ValueKind::Raw,
            "--config",
            "=PATH",
            &["Path to the Rigor configuration file"],
        ),
        Switch::new(
            "stats",
            &[("stats", false), ("no-stats", true)],
            ArgStyle::Flag,
            ValueKind::Raw,
            "--[no-]stats",
            "",
            &["Print run summary"],
        ),
        Switch::new(
            "workers",
            &[("workers", false)],
            ArgStyle::Required,
            ValueKind::Int,
            "--workers",
            "=N",
            &["workers"],
        ),
        Switch::new(
            "format",
            &[("format", false)],
            ArgStyle::Required,
            ValueKind::Choice(&["text", "json"]),
            "--format",
            "=FORMAT",
            &["format"],
        ),
    ];
    const P: OptParser = OptParser::new("Usage: t [options]", TBL);

    fn argv(s: &str) -> Vec<String> {
        s.split(' ').map(str::to_string).collect()
    }

    fn items(p: Parsed) -> Vec<Item> {
        match p {
            Parsed::Items(i) => i,
            Parsed::Exit { stderr, code, .. } => panic!("exit {code}: {stderr}"),
        }
    }

    #[test]
    fn abbrev_and_equals() {
        let i = items(P.parse(&argv("--conf=x.yml --form=j f.rb")));
        assert!(matches!(&i[0], Item::Opt { key: "config", value: Some(Value::Str(v)), .. } if v == "x.yml"));
        assert!(matches!(&i[1], Item::Opt { key: "format", value: Some(Value::Str(v)), .. } if v == "json"));
        assert!(matches!(&i[2], Item::Positional(p) if p == "f.rb"));
    }

    #[test]
    fn terminator_and_permute() {
        let i = items(P.parse(&argv("a.rb -- --stats b.rb")));
        assert!(matches!(&i[0], Item::Positional(p) if p == "a.rb"));
        assert!(matches!(&i[1], Item::Positional(p) if p == "--stats"));
        assert!(matches!(&i[2], Item::Positional(p) if p == "b.rb"));
    }

    #[test]
    fn needless_flag_value() {
        match P.parse(&argv("--stats=x")) {
            Parsed::Exit { stderr, code, .. } => {
                assert_eq!(code, 64);
                assert!(stderr.starts_with("needless argument: --stats=x"));
            }
            _ => panic!(),
        }
    }

    #[test]
    fn missing_required() {
        match P.parse(&argv("--config")) {
            Parsed::Exit { stderr, code, .. } => {
                assert_eq!(code, 64);
                assert!(stderr.starts_with("missing argument: --config"));
            }
            _ => panic!(),
        }
    }

    #[test]
    fn invalid_value_pair_form() {
        match P.parse(&argv("--workers bogus")) {
            Parsed::Exit { stderr, code, .. } => {
                assert_eq!(code, 64);
                assert_eq!(stderr, "invalid argument: --workers bogus\n");
            }
            _ => panic!(),
        }
    }
}
