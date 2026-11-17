//! The ADR-0045 conventions, enforced (rigor-rs#429):
//!
//! 1. Every production parse of Ruby source goes through
//!    `rigor_parse::parse_bounded` — never a bare `rigor_parse::parse` or
//!    `ruby_prism::parse`, which skips the MAX_NESTING_DEPTH bound.
//! 2. Every analysis thread is built through `threads`' machinery
//!    (ANALYSIS_STACK_BYTES with its fallback), never a bare
//!    `thread::spawn`, `thread::scope`, `thread::Builder` or a
//!    `rayon::ThreadPoolBuilder` that does not route workers through
//!    `spawn_pool_thread`.
//!
//! The deep-nesting tests cover only the call sites that exist today; a new
//! subcommand or helper that skips either convention brings back the stack
//! overflow on deep input. This audit reads every `crates/*/src/**/*.rs` as
//! text — comments and literals blanked, so a `thread::spawn(` in a doc
//! comment or string is not a hit — and fails on:
//!
//! - the unbounded parse entry points (`rigor_parse::parse`,
//!   `ruby_prism::parse`, and `crate::`/`self::`/`super::parse` inside
//!   rigor-parse) — called or taken as a value — or a `use` importing
//!   the bare `parse` leaf;
//! - `ThreadPoolBuilder::new()`, `thread::Builder::new()`, `thread::spawn`
//!   or `thread::scope` — called or taken as a value — unless the same
//!   builder chain names `spawn_pool_thread` or `ANALYSIS_STACK_BYTES`,
//!   or the site is in the `threads` helper module itself.
//!
//! Call patterns are token sequences: whitespace around `::` and before
//! `(` (legal Rust, and `cargo fmt --check` is not a gate here) does not
//! bypass them, `spawn`/`scope` flag with or without `::<…>` turbofish or
//! an immediate call, and a `use` that aliases (`… as x`) or globs
//! (`…::*`) a flagged root flags too.
//!
//! `#[cfg(test)]` items are skipped, as are the files `#[cfg(test)] mod x;`
//! declarations resolve to. Everything else is production code: a hit must
//! be justified by an [`ALLOW_LIST`] entry carrying a one-line reason. An
//! entry pins the exact hit count, so a moved, duplicated or removed site
//! fails the audit until its entry is updated.
//!
//! `rayon::spawn`/`rayon::scope`/`par_iter` are NOT flagged: their work
//! lands on the global pool, which `configure_global_pool` has already
//! given the analysis stack. Only thread CREATION can bypass it.
//!
//! Modelled on `rigor-parse`'s `family_inventory.rs` — `blank`, `close_of`
//! and the identifier helpers are copied verbatim.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// The analysis-thread helper module — the one place builders and spawns
/// may live unflagged.
const THREADS_MODULE: &str = "rigor-cli/src/threads.rs";

/// (file suffix under `crates/`, flagged tag, exact hit count, reason).
/// The count is exact: a new hit of the same tag in the same file fails,
/// and a removed hit leaves a stale entry that fails too.
const ALLOW_LIST: &[(&str, &str, usize, &str)] = &[
    (
        "rigor-parse/src/lib.rs",
        "ruby_prism::parse",
        1,
        "the unbounded entry point's own definition",
    ),
    (
        "rigor-parse/src/depth.rs",
        "ruby_prism::parse",
        1,
        "parse_bounded's own parse — the depth bound runs on its result",
    ),
    (
        "rigor-parse/src/depth.rs",
        "thread::scope",
        1,
        "the refused-tree release: its thread's stack is sized from the source length, not \
         ANALYSIS_STACK_BYTES",
    ),
    (
        "rigor-parse/src/depth.rs",
        "thread::Builder::new(",
        1,
        "same refused-tree release thread — `.stack_size(stack)` scales with the source",
    ),
    (
        "rigor-cli/src/sidecar.rs",
        "thread::spawn",
        1,
        "the sidecar handshake reader: bounded pipe IO, no analysis",
    ),
];

/// Unbounded-parse call patterns (token sequences — whitespace around
/// `::` or before `(` cannot bypass them). No trailing `(`: a function
/// value (`let f = rigor_parse::parse`) is the same hazard. `crate::`/
/// `self::`/`super::` qualify only inside rigor-parse, where they can
/// reach the crate-root `parse`.
const PARSE_CALLS: &[&[&str]] = &[
    &["rigor_parse", "::", "parse"],
    &["ruby_prism", "::", "parse"],
];
const PARSE_CALLS_INTERNAL: &[&[&str]] = &[
    &["crate", "::", "parse"],
    &["self", "::", "parse"],
    &["super", "::", "parse"],
];

/// Thread-creation patterns. `ThreadPoolBuilder::new` and
/// `thread::Builder::new` pass when their builder chain sets the analysis
/// stack. `thread::spawn`/`thread::scope` match WITHOUT the `(` — a
/// turbofish (`spawn::<F,T>`) or a bare function value
/// (`let f = thread::spawn`) is the same hazard — so they always need an
/// allow-list entry outside `THREADS_MODULE`.
const THREAD_BUILDERS: &[&[&str]] = &[
    &["ThreadPoolBuilder", "::", "new", "("],
    &["thread", "::", "Builder", "::", "new", "("],
];
const THREAD_SPAWNS: &[&[&str]] = &[&["thread", "::", "spawn"], &["thread", "::", "scope"]];

/// Builder-chain tokens that prove the analysis stack is set.
const STACK_PROOF: &[&str] = &["spawn_pool_thread", "ANALYSIS_STACK_BYTES"];

/// `use`-tree leaves that smuggle a flagged symbol past the call-site
/// patterns: `use rigor_parse::parse` then `parse(src)`, `use
/// std::thread::spawn` then `spawn(job)`.
const USE_LEAVES_THREAD: &[&str] = &["spawn", "scope", "Builder"];

#[test]
fn production_code_uses_bounded_parse_and_analysis_threads() {
    let crates = workspace_root().join("crates");
    let mut files = Vec::new();
    for entry in fs::read_dir(&crates).unwrap_or_else(|e| panic!("{crates:?}: {e}")) {
        let src = entry.unwrap().path().join("src");
        if src.is_dir() {
            collect_rs(&src, &mut files);
        }
    }
    files.sort();

    // Pass 1: files `#[cfg(test)] mod x;` declarations resolve to are
    // test-only and skipped whole, like the inline `#[cfg(test)]` items.
    let mut test_only = Vec::new();
    for path in &files {
        let text = blank(&fs::read_to_string(path).unwrap_or_else(|e| panic!("{path:?}: {e}")));
        for (open, close) in cfg_test_spans(&text) {
            collect_test_mods(&text, open, close, path, &mut test_only);
        }
    }

    let mut findings = Vec::new();
    for path in &files {
        let rel = rel_path(path);
        if rel == THREADS_MODULE || test_only.contains(path) {
            continue;
        }
        let text = blank(&fs::read_to_string(path).unwrap_or_else(|e| panic!("{path:?}: {e}")));
        let spans = cfg_test_spans(&text);
        let in_test = |at: usize| spans.iter().any(|&(s, e)| at >= s && at < e);
        let uses = use_item_spans(&text);
        let uses_active: Vec<(usize, usize)> =
            uses.iter().copied().filter(|&(s, _)| !in_test(s)).collect();
        let active = |at: usize| {
            !in_test(at) && !uses.iter().any(|&(s, e)| at >= s && at < e)
        };
        let internal = rel.starts_with("rigor-parse/");
        let mut hits: Vec<(usize, String)> = Vec::new();
        let parse_calls: &[&[&str]] = if internal { PARSE_CALLS_INTERNAL } else { &[] };
        for &pat in PARSE_CALLS.iter().chain(parse_calls.iter()) {
            let tag = pattern_tag(pat);
            hits.extend(find_seq(&text, pat, &active).into_iter().map(|(at, _)| (at, tag.clone())));
        }
        for &pat in THREAD_SPAWNS {
            let tag = pattern_tag(pat);
            hits.extend(find_seq(&text, pat, &active).into_iter().map(|(at, _)| (at, tag.clone())));
        }
        for &pat in THREAD_BUILDERS {
            let tag = pattern_tag(pat);
            hits.extend(
                find_seq(&text, pat, &active)
                    .into_iter()
                    .filter(|&(_, end)| !chain_has_stack_proof(&text, end))
                    .map(|(at, _)| (at, tag.clone())),
            );
        }
        hits.extend(flagged_uses(&text, &uses_active, internal));
        for (at, tag) in hits {
            findings.push(format!("{rel}:{}: `{tag}` — {}", line_of(&text, at), advice(&tag)));
        }
    }
    check_allow_list(&mut findings);
    assert!(
        findings.is_empty(),
        "analysis-stack audit (rigor-rs#429, ADR-0045): every production parse must be \
         `parse_bounded` and every analysis thread must come from `threads`' helpers:\n{}",
        findings.join("\n")
    );
}

/// The advice each tag's finding ends with.
fn advice(tag: &str) -> &'static str {
    if tag.contains("parse") {
        "parse Ruby source through `rigor_parse::parse_bounded` (the depth bound, rigor-rs#412); \
         allow-list this site in analysis_stack_audit.rs only if it IS the wrapper"
    } else if tag.starts_with("use") {
        "import from the safe path instead — `parse_bounded`, or rigor-cli `threads`' helpers — or \
         allow-list the import in analysis_stack_audit.rs with a one-line reason"
    } else {
        "analysis threads are built through rigor-cli `threads`' helpers (ANALYSIS_STACK_BYTES + \
         fallback): route the builder chain through `spawn_pool_thread`/`ANALYSIS_STACK_BYTES`, or \
         allow-list the site in analysis_stack_audit.rs with a one-line reason"
    }
}

/// Consume the allow-listed hits; a leftover finding or a stale entry (a
/// site that moved or shrank) stays in `findings` as a failure.
fn check_allow_list(findings: &mut Vec<String>) {
    let mut wanted: BTreeMap<(&str, &str), usize> = ALLOW_LIST
        .iter()
        .map(|&(file, tag, count, _)| ((file, tag), count))
        .collect();
    findings.retain(|f| {
        for ((file, tag), left) in wanted.iter_mut() {
            if *left > 0 && f.starts_with(&format!("{file}:")) && f.contains(&format!("`{tag}`")) {
                *left -= 1;
                return false;
            }
        }
        true
    });
    for ((file, tag), left) in wanted {
        if left > 0 {
            findings.push(format!(
                "{file}: stale ALLOW_LIST entry: `{tag}` allows {left} more hit(s) than exist — \
                 update or drop the entry"
            ));
        }
    }
}

/// Every `.rs` file under `dir`, recursively.
fn collect_rs(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap_or_else(|e| panic!("{dir:?}: {e}")) {
        let path = entry.unwrap().path();
        if path.is_dir() {
            collect_rs(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path.canonicalize().unwrap_or(path));
        }
    }
}

/// `crates/<crate>/src/…` reduced to `<crate>/src/…` for matching and output.
fn rel_path(path: &Path) -> String {
    let s = path.to_string_lossy();
    let s = s.rsplit("/crates/").next().unwrap_or(&s);
    s.replace('\\', "/")
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().expect("workspace root")
}

/// 1-based line of byte `at` (the blanked text keeps every newline).
fn line_of(text: &str, at: usize) -> usize {
    text[..at].matches('\n').count() + 1
}

/// The tag a call pattern reports under (its tokens joined —
/// `thread::spawn`, `rigor_parse::parse(`).
fn pattern_tag(pat: &[&str]) -> String {
    pat.concat()
}

/// `tokens` as a token sequence on blanked text — whitespace allowed
/// between tokens (`std :: thread :: spawn (f)` cannot bypass a pattern,
/// and `cargo fmt --check` is not a gate here). Identifier tokens must
/// match whole identifiers (`spawn` does not match `spawn_pool_thread`,
/// `thread` does not match `threads`). Returns `(match start, index just
/// after the last token)` for hits outside skipped spans.
///
/// The head's left boundary: a path qualifier (`::`) or any non-identifier,
/// non-`.` byte — `mythread::spawn`, `pool_thread::spawn` and
/// `x.thread::…` field syntax are out, `std::thread::spawn`,
/// `::std::thread::spawn` and `pool::thread::spawn` are in.
fn find_seq(text: &str, tokens: &[&str], active: &dyn Fn(usize) -> bool) -> Vec<(usize, usize)> {
    let b = text.as_bytes();
    let ident_tok = |t: &&str| t.bytes().all(is_ident_byte);
    let mut out = Vec::new();
    'next: for (at, _) in text.match_indices(tokens[0]) {
        match at.checked_sub(1).map(|i| b[i]) {
            None => {}
            Some(b':') => {}
            Some(c) if !is_ident_byte(c) && c != b'.' => {}
            _ => continue 'next,
        }
        let mut end = at;
        for tok in tokens {
            end = skip_ws(b, end);
            if !b[end..].starts_with(tok.as_bytes()) {
                continue 'next;
            }
            end += tok.len();
            if ident_tok(tok) && b.get(end).is_some_and(|&c| is_ident_byte(c)) {
                continue 'next;
            }
        }
        if active(at) {
            out.push((at, end));
        }
    }
    out
}

/// Whether the builder chain ending a `…Builder::new(` hit sets the
/// analysis stack: `end` is just after `new(`; match it, then consume
/// `.name(…)` links and look for `spawn_pool_thread` or
/// `ANALYSIS_STACK_BYTES` as bare tokens in the chain text.
fn chain_has_stack_proof(text: &str, end: usize) -> bool {
    let b = text.as_bytes();
    let open = end - 1; // the `(` the pattern ended on
    let mut last = close_of(b, open) + 1;
    loop {
        let mut i = skip_ws(b, last);
        if i >= b.len() || b[i] != b'.' {
            break;
        }
        i = skip_ws(b, i + 1);
        i += ident_at(text, i).len();
        i = skip_ws(b, i);
        if i >= b.len() || b[i] != b'(' {
            break; // a field access ends the call chain
        }
        last = close_of(b, i) + 1;
    }
    let chain = &text[open..last];
    STACK_PROOF.iter().any(|p| {
        chain.match_indices(p).any(|(i, _)| {
            let before = i == 0 || !is_ident_byte(chain.as_bytes()[i - 1]);
            let after = chain.as_bytes().get(i + p.len()).is_none_or(|&c| !is_ident_byte(c));
            before && after
        })
    })
}

/// Every `use` item's byte extent outside `#[cfg(test)]` spans — so call
/// patterns do not also report the sites a `use:*` tag covers. `uses` and
/// the `#[cfg(test)]` spans are computed once per file by the caller.
fn use_item_spans(text: &str) -> Vec<(usize, usize)> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    for (at, _) in text.match_indices("use") {
        let before = at == 0 || !is_ident_byte(b[at - 1]);
        let after = b.get(at + 3).is_none_or(|&c| !is_ident_byte(c));
        if before && after {
            if let Some(semi) = text[at..].find(';').map(|n| at + n) {
                out.push((at, semi + 1));
            }
        }
    }
    out
}

/// `use` items (outside `#[cfg(test)]` spans — the caller's `active` mask
/// already excludes them AND the use-item extents it also hands in)
/// reaching a flagged symbol:
///
/// - a `parse` leaf under `rigor_parse` — plus `crate`/`self`/`super`
///   inside rigor-parse, where they reach the crate root — `use
///   rigor_parse::parse`, `use rigor_parse::{lower, parse}`;
/// - a `spawn`/`scope`/`Builder` leaf under a `thread` segment (`use
///   std::thread::spawn`, `use std::thread::{spawn}`), plus the same
///   under any deeper path (`crossbeam::thread::scope`);
/// - an ALIAS or GLOB that renames a flagged root out of the call
///   patterns' reach: `use rigor_parse as rp` (`rp::parse`),
///   `use rigor_parse::{self as rp}`, `use std::thread as t`
///   (`t::spawn`), `use std::thread::{self as t}`,
///   `use rayon::ThreadPoolBuilder as TPB`, `use rigor_parse::*`
///   (`parse(src)`), `use std::thread::*` (`spawn(job)`).
fn flagged_uses(
    text: &str,
    uses: &[(usize, usize)],
    internal: bool,
) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    for &(at, end) in uses {
        let item = &text[at + 3..end - 1];
        let toks: Vec<(usize, &str)> = ident_spans(item).collect();
        let roots: &[&str] =
            if internal { &["rigor_parse", "crate", "self", "super"] } else { &["rigor_parse"] };
        let first = toks.first().map_or("", |t| t.1);
        let pair = |x: &str, y: &str| {
            toks.windows(2).any(|w| w[0].1 == x && w[1].1 == y)
        };
        if roots.contains(&first) {
            if toks.iter().any(|t| t.1 == "parse") {
                out.push((at, "use:parse".to_string()));
            }
            // `use rigor_parse as rp`, `use rigor_parse::{self as rp}`,
            // `use rigor_parse::*` — the calls then escape the patterns.
            if toks.get(1).is_some_and(|t| t.1 == "as")
                || pair("self", "as")
                || after_ident_is_glob(item, toks[0])
            {
                out.push((at, "use:alias-or-glob".to_string()));
            }
        }
        if toks.iter().any(|t| t.1 == "thread") {
            for &leaf in USE_LEAVES_THREAD {
                if toks.iter().any(|t| t.1 == leaf) {
                    out.push((at, format!("use:{leaf}")));
                }
            }
            // `use std::thread as t`, `use std::thread::{self as t}`,
            // `use std::thread::*` — `t::spawn`/`spawn` escape the patterns.
            if pair("thread", "as")
                || pair("self", "as")
                || toks.iter().any(|t| t.1 == "thread" && after_ident_is_glob(item, *t))
            {
                out.push((at, "use:alias-or-glob".to_string()));
            }
        }
        // `use rayon::ThreadPoolBuilder as TPB` — `TPB::new()` escapes.
        if pair("ThreadPoolBuilder", "as") {
            out.push((at, "use:alias-or-glob".to_string()));
        }
    }
    out
}

/// The identifiers of `s` with their byte offsets, in order (on blanked
/// text: no comments or literals can produce phantom ones).
fn ident_spans(s: &str) -> impl Iterator<Item = (usize, &str)> + '_ {
    let b = s.as_bytes();
    (0..b.len())
        .filter(move |&i| is_ident_byte(b[i]) && (i == 0 || !is_ident_byte(b[i - 1])))
        .map(move |i| (i, ident_at(s, i)))
}

/// Whether `tok` (an identifier span in `s`) is followed by `::*` — a glob
/// leaf: `rigor_parse::*`, `std::thread::*`.
fn after_ident_is_glob(s: &str, tok: (usize, &str)) -> bool {
    let b = s.as_bytes();
    let mut i = skip_ws(b, tok.0 + tok.1.len());
    if !b[i..].starts_with(b"::") {
        return false;
    }
    i = skip_ws(b, i + 2);
    b.get(i) == Some(&b'*')
}

/// The `#[cfg(test)]` items' byte extents: the `#` through the item's `;`
/// or the `}` closing its `{` — whichever comes first at bracket depth 0.
/// `#[…]` attributes between the attribute and its item are skipped too.
/// `cfg(all(test, …))` is not recognised — none exist; a new one needs a
/// case here rather than an allow-list entry.
fn cfg_test_spans(text: &str) -> Vec<(usize, usize)> {
    let b = text.as_bytes();
    let mut spans = Vec::new();
    let mut i = 0;
    while i < b.len() {
        i = match b[i] {
            b'#' => match cfg_test_attr(b, i) {
                Some(end) => {
                    let start = item_start(b, end);
                    let close = item_end(b, start);
                    spans.push((i, close));
                    close
                }
                None => i + 1,
            },
            _ => i + 1,
        };
    }
    spans
}

/// `#[ cfg ( test ) ]` at `at` (the `#`): the index just past `]`.
fn cfg_test_attr(b: &[u8], at: usize) -> Option<usize> {
    let i = eat(b, at + 1, "[")?;
    let i = eat_ident(b, i, "cfg")?;
    let i = eat(b, i, "(")?;
    let i = eat_ident(b, i, "test")?;
    let i = eat(b, i, ")")?;
    eat(b, i, "]")
}

/// The attributed item's first byte: skip `#[…]` attributes that follow
/// `#[cfg(test)]` itself (`#[cfg(test)] #[allow(…)] mod x;`).
fn item_start(b: &[u8], mut i: usize) -> usize {
    loop {
        i = skip_ws(b, i);
        if b.get(i) == Some(&b'#') {
            if let Some(open) = eat(b, i + 1, "[").map(|after| after - 1) {
                i = close_of(b, open) + 1;
                continue;
            }
        }
        return i;
    }
}

/// The byte after an item: `;` at bracket depth 0 (`mod x;`, `use a;`,
/// `static x: T = v;`), else the `}` matching the first depth-0 `{`
/// (`mod x {…}`, `fn`, `impl`, a block expression). End of file counts as
/// the end.
fn item_end(b: &[u8], from: usize) -> usize {
    let mut depth = 0usize;
    for i in from..b.len() {
        match b[i] {
            b'(' | b'[' => depth += 1,
            b')' | b']' => depth = depth.saturating_sub(1),
            b';' if depth == 0 => return i + 1,
            b'{' if depth == 0 => return close_of(b, i) + 1,
            _ => {}
        }
    }
    b.len()
}

/// `mod name;` declarations inside a `#[cfg(test)]` item, resolved to file
/// paths. `mod a { … }` blocks recurse so `mod a { mod b; }` resolves
/// `…/a/b.rs`. The file's own module dir is the base: `foo.rs` nests its
/// children under `foo/`; `lib.rs`/`main.rs`/`mod.rs` keep them alongside.
fn collect_test_mods(text: &str, open: usize, close: usize, file: &Path, out: &mut Vec<PathBuf>) {
    let mut base = file.parent().unwrap().to_path_buf();
    let name = file.file_stem().unwrap().to_string_lossy();
    if name != "lib" && name != "main" && name != "mod" {
        base.push(name.as_ref());
    }
    let mut stack: Vec<String> = Vec::new();
    let mut depth = 0usize;
    let b = text.as_bytes();
    let mut i = open;
    while i < close {
        match b[i] {
            b'{' => depth += 1,
            b'}' => {
                depth = depth.saturating_sub(1);
                stack.truncate(depth);
            }
            _ => {
                if text[i..].starts_with("mod")
                    && b.get(i + 3).is_none_or(|&c| !is_ident_byte(c))
                    && (i == open || !is_ident_byte(b[i - 1]))
                {
                    let mut j = skip_ws(b, i + 3);
                    let name = ident_at(text, j);
                    j = skip_ws(b, j + name.len());
                    match b.get(j) {
                        Some(&b';') => {
                            let dir = stack.iter().fold(base.clone(), |mut d, m| {
                                d.push(m);
                                d
                            });
                            for cand in
                                [dir.join(format!("{name}.rs")), dir.join(name).join("mod.rs")]
                            {
                                if cand.is_file() {
                                    out.push(cand.canonicalize().unwrap_or(cand));
                                }
                            }
                        }
                        Some(&b'{') => stack.push(name.to_string()),
                        _ => {}
                    }
                }
            }
        }
        i += 1;
    }
}

fn skip_ws(b: &[u8], mut i: usize) -> usize {
    while i < b.len() && b[i].is_ascii_whitespace() {
        i += 1;
    }
    i
}

/// `tok` at `i` after whitespace: the index just past it.
fn eat(b: &[u8], i: usize, tok: &str) -> Option<usize> {
    let i = skip_ws(b, i);
    b[i..].starts_with(tok.as_bytes()).then_some(i + tok.len())
}

/// The identifier `want` at `i` after whitespace (a longer identifier —
/// `cfg_attr`, `tests` — does not match).
fn eat_ident(b: &[u8], i: usize, want: &str) -> Option<usize> {
    let i = skip_ws(b, i);
    let end = i + want.len();
    (b[i..].starts_with(want.as_bytes())
        && b.get(end).is_none_or(|&c| !is_ident_byte(c)))
    .then_some(end)
}

/// The index of the bracket that closes the one at `open`.
fn close_of(s: &[u8], open: usize) -> usize {
    let mut depth = 0usize;
    for (i, &c) in s.iter().enumerate().skip(open) {
        match c {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => {
                depth -= 1;
                if depth == 0 {
                    return i;
                }
            }
            _ => {}
        }
    }
    panic!("unbalanced bracket at byte {open}")
}

/// The identifier starting at byte `i` of `s` (empty if none).
fn ident_at(s: &str, i: usize) -> &str {
    let tail = &s[i..];
    let len = tail.bytes().position(|c| !is_ident_byte(c)).unwrap_or(tail.len());
    &tail[..len]
}

fn is_ident_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

/// `src` with every comment, and the contents of every string and char
/// literal, overwritten with spaces. Newlines survive, so offsets and line
/// numbers still match `src`. Copied verbatim from
/// `rigor-parse`'s `family_inventory.rs`.
fn blank(src: &str) -> String {
    let b = src.as_bytes();
    let mut out = b.to_vec();
    let mut wipe = |from: usize, to: usize| {
        for c in &mut out[from..to] {
            if *c != b'\n' {
                *c = b' ';
            }
        }
    };
    // Whether byte `j` starts a token: no identifier byte runs into it.
    let starts = |j: usize| j == 0 || !is_ident_byte(b[j - 1]);
    let mut i = 0;
    while i < b.len() {
        // An `r` starts a raw string alone, or after a `b` or `c` that
        // starts its token (`br"…"`, `cr"…"`), never inside an identifier.
        let prefix = starts(i) || (b"bc".contains(&b[i - 1]) && starts(i - 1));
        let raw = if b[i] == b'r' && prefix { raw_quote(&b[i + 1..]) } else { None };
        if b[i..].starts_with(b"//") {
            let end = src[i..].find('\n').map_or(b.len(), |n| i + n);
            wipe(i, end);
            i = end;
        } else if b[i..].starts_with(b"/*") {
            let (start, mut depth) = (i, 0usize);
            loop {
                if b[i..].starts_with(b"/*") {
                    depth += 1;
                    i += 2;
                } else if b[i..].starts_with(b"*/") {
                    depth -= 1;
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    i += 1;
                }
            }
            wipe(start, i);
        } else if let Some(hashes) = raw {
            // `r"…"` / `r#"…"#`, also after `b` or `c`: no escapes, ends at
            // `"` plus as many `#`.
            let body = i + 2 + hashes;
            let mut closing = vec![b'"'];
            closing.extend(std::iter::repeat_n(b'#', hashes));
            let end = body + src[body..].find(std::str::from_utf8(&closing).unwrap()).unwrap();
            wipe(body, end);
            i = end + closing.len();
        } else if b[i] == b'"' {
            let mut j = i + 1;
            while b[j] != b'"' {
                j += if b[j] == b'\\' { 2 } else { 1 };
            }
            wipe(i + 1, j);
            i = j + 1;
        } else if b[i] == b'\'' {
            // A char literal (`'('`, `'\''`, `'é'`), else a lifetime or label.
            let len = if b[i + 1] == b'\\' {
                b[i + 3..].iter().position(|&c| c == b'\'').map(|n| n + 2)
            } else {
                let c = src[i + 1..].chars().next().map_or(0, char::len_utf8);
                (b.get(i + 1 + c) == Some(&b'\'')).then_some(c)
            };
            match len {
                Some(len) => {
                    wipe(i + 1, i + 1 + len);
                    i += len + 2;
                }
                None => i += 1,
            }
        } else {
            i += 1;
        }
    }
    String::from_utf8(out).expect("blanking keeps whole UTF-8 sequences")
}

/// For `#…#"` at the start of `s`, the number of `#`s.
fn raw_quote(s: &[u8]) -> Option<usize> {
    let hashes = s.iter().take_while(|&&c| c == b'#').count();
    (s.get(hashes) == Some(&b'"')).then_some(hashes)
}
