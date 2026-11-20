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
//! 3. `fn main` never builds the rayon pool itself (rigor-rs#431/#676):
//!    the pool is built from the command thread's body, after that
//!    thread has reserved its stack, so workers cannot spend the
//!    reservation budget first. `main_never_builds_the_pool_itself`
//!    fails on a `configure_global_pool` call or any `ThreadPoolBuilder`
//!    in `fn main`'s own body — no allow-list, because a stack-proven
//!    builder there is still the inversion, a shape the repo-wide pass
//!    above passes. A `use` in that body that aliases or globs a pool
//!    token is a finding too (rigor-rs#724) — it would rename the call
//!    site out of the token matcher's reach.
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
//! - `ThreadPoolBuilder::new()`/`::default()`, `thread::Builder::new()`,
//!   `thread::spawn` or `thread::scope` — called or taken as a value —
//!   unless the same builder chain names `spawn_pool_thread` or
//!   `ANALYSIS_STACK_BYTES`, or the site is in the `threads` helper
//!   module itself.
//!
//! Call patterns are token sequences: whitespace around `::` and before
//! `(` (legal Rust, and `cargo fmt --check` is not a gate here) does not
//! bypass them, `spawn`/`scope` flag with or without `::<…>` turbofish or
//! an immediate call, and a `use` that aliases (`… as x`) or globs
//! (`…::*`, `…::{*}`, `…::{a, *}`) a flagged root flags too.
//!
//! `#[cfg(test)]` items are skipped, as are the files `#[cfg(test)] mod x;`
//! declarations resolve to. A `#[cfg(test)]` that does not open an item
//! (a field, variant, parameter or match arm — including a `pub`-qualified
//! field, whose `pub` is itself an item keyword) masks only itself; a
//! keyword-headed one (a field or parameter named `union`) masks no
//! further than the next `)`, `]` or `}` the position never opened.
//! Either way, whatever follows it stays production. Everything else is
//! production code: a hit must
//! be justified by an [`ALLOW_LIST`] entry carrying a one-line reason. An
//! entry pins the exact hit count, so a moved, duplicated or removed site
//! fails the audit until its entry is updated.
//!
//! `rayon::spawn`/`rayon::scope`/`par_iter` are NOT flagged: their work
//! lands on the global pool, which `configure_global_pool` (the first step
//! of the command thread, rigor-rs#431) has already given the analysis
//! stack. Only thread CREATION can bypass it.
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
    &["ThreadPoolBuilder", "::", "default", "("],
    &["thread", "::", "Builder", "::", "new", "("],
];
const THREAD_SPAWNS: &[&[&str]] = &[&["thread", "::", "spawn"], &["thread", "::", "scope"]];

/// Builder-chain tokens that prove the analysis stack is set.
const STACK_PROOF: &[&str] = &["spawn_pool_thread", "ANALYSIS_STACK_BYTES"];

/// `use`-tree leaves that smuggle a flagged symbol past the call-site
/// patterns: `use rigor_parse::parse` then `parse(src)`, `use
/// std::thread::spawn` then `spawn(job)`.
const USE_LEAVES_THREAD: &[&str] = &["spawn", "scope", "Builder"];

/// Pool-building tokens that may not appear in `fn main`'s own body
/// (rigor-rs#676): the pool is built from the command thread's body, so
/// the command's stack is already reserved when workers take theirs. No
/// trailing `(` on either — a function value or a bare builder name is
/// the same hazard as a call — and no builder-chain exemption: in `fn
/// main`, the pool must not be built at all.
const MAIN_POOL_TOKENS: &[&str] = &["configure_global_pool", "ThreadPoolBuilder"];

/// The path segments a `use` glob inside `fn main`'s body reaches a pool
/// token through (rigor-rs#724): `threads` holds
/// `configure_global_pool`, `rayon` holds `ThreadPoolBuilder`. A glob of
/// any other path is beyond what a text matcher can resolve — the same
/// fixed-root limit `flagged_uses` discloses.
const POOL_USE_ROOTS: &[&str] = &["threads", "rayon"];

/// The advice every pool-ordering finding carries. There is no allow-list
/// entry for these: a hit in `fn main` IS the rigor-rs#431 inversion, so
/// the ordering can only regress by editing this audit — or by deliberate
/// indirection it cannot see, a helper `fn` defined OUTSIDE `fn main`'s
/// body that makes the pool call for it (rigor-rs#724).
const MAIN_POOL_ADVICE: &str = "the rayon pool is built from the command thread's body \
     (threads::run_command_thread(run_command_with_pool)), after that thread has reserved its \
     stack — building or configuring it in fn main lets the workers spend the reservation budget \
     first (the rigor-rs#431 inversion); move the call into the command body";

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
/// - a `parse` leaf under `rigor_parse` or `ruby_prism` — plus
///   `crate`/`self`/`super` inside rigor-parse, where they reach the
///   crate root — `use rigor_parse::parse`, `use rigor_parse::{lower,
///   parse}`, `use ruby_prism::{ParseResult, parse}`;
/// - a `spawn`/`scope`/`Builder` leaf under a `thread` segment (`use
///   std::thread::spawn`, `use std::thread::{spawn}`), plus the same
///   under any deeper path (`crossbeam::thread::scope`);
/// - a `ruby_prism` segment aliased or globbed at a deeper position —
///   `use rigor_parse::ruby_prism as prism`, `use
///   rigor_parse::ruby_prism::*` — through lib.rs's `pub use ruby_prism;`;
/// - an ALIAS or GLOB that renames a flagged root out of the call
///   patterns' reach: `use rigor_parse as rp` (`rp::parse`),
///   `use rigor_parse::{self as rp}`, `use ruby_prism as prism`
///   (`prism::parse`), `use rigor_parse::ruby_prism as prism` (through
///   lib.rs's `pub use ruby_prism;`), `use std::thread as t`
///   (`t::spawn`), `use std::thread::{self as t}`,
///   `use rayon::ThreadPoolBuilder as TPB`, `use rigor_parse::*`
///   (`parse(src)`), `use std::thread::*` (`spawn(job)`), and the same
///   globs written with braces (`use rigor_parse::{*}`,
///   `use std::thread::{a, *}`) — rustfmt collapses `::{*}` to `::*`,
///   but `cargo fmt --check` is not a gate (rigor-rs#652).
fn flagged_uses(
    text: &str,
    uses: &[(usize, usize)],
    internal: bool,
) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    for &(at, end) in uses {
        let item = &text[at + 3..end - 1];
        let toks: Vec<(usize, &str)> = ident_spans(item).collect();
        let roots: &[&str] = if internal {
            &["rigor_parse", "ruby_prism", "crate", "self", "super"]
        } else {
            &["rigor_parse", "ruby_prism"]
        };
        let first = toks.first().map_or("", |t| t.1);
        let pair = |x: &str, y: &str| {
            toks.windows(2).any(|w| w[0].1 == x && w[1].1 == y)
        };
        if roots.contains(&first) {
            if toks.iter().any(|t| t.1 == "parse") {
                out.push((at, "use:parse".to_string()));
            }
            // `use rigor_parse as rp`, `use rigor_parse::{self as rp}`,
            // `use rigor_parse::*` / `use rigor_parse::{*}` — the calls
            // then escape the patterns.
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
            // `use std::thread::*` / `use std::thread::{*}` —
            // `t::spawn`/`spawn` escape the patterns.
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
        // `ruby_prism` at a deeper path position — `use rigor_parse::
        // ruby_prism as prism` through lib.rs's `pub use ruby_prism;`, or
        // `use rigor_parse::ruby_prism::*` — hides the root the call
        // patterns key on. (A first-position `ruby_prism` is covered by
        // the `roots` branch above.)
        if first != "ruby_prism"
            && (pair("ruby_prism", "as")
                || toks.iter().any(|t| t.1 == "ruby_prism" && after_ident_is_glob(item, *t)))
        {
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

/// Whether `tok` (an identifier span in `s`) is followed by a glob of
/// that ident: `rigor_parse::*`, `std::thread::*`, the brace spelling
/// `::{*}` (rigor-rs#652), or a mixed list that still globs
/// (`::{a, *}`, `::{*, a}`). Nested child globs (`::{a::*}`,
/// `::{a::{*}}`) are not a glob of `tok` itself. Nested anonymous
/// groups (`::{{*}}`) still glob `tok` in Rust's grammar — they stay
/// a disclosed limit of this matcher; a use-tree flattener would close
/// them (rigor-rs#654).
fn after_ident_is_glob(s: &str, tok: (usize, &str)) -> bool {
    let b = s.as_bytes();
    let mut i = skip_ws(b, tok.0 + tok.1.len());
    if !b[i..].starts_with(b"::") {
        return false;
    }
    i = skip_ws(b, i + 2);
    match b.get(i) {
        Some(&b'*') => true,
        Some(&b'{') => brace_group_has_self_glob(b, i),
        _ => false,
    }
}

/// A `*` list item of the `{...}` group at `open`, not a nested `a::*`.
fn brace_group_has_self_glob(b: &[u8], open: usize) -> bool {
    let close = close_of(b, open);
    let mut depth = 0usize;
    let mut i = open;
    while i < close {
        match b[i] {
            b'{' => {
                depth += 1;
                i += 1;
            }
            b'}' => {
                depth = depth.saturating_sub(1);
                i += 1;
            }
            b'*' if depth == 1 && !preceded_by_colon_colon(b, i) => return true,
            _ => i += 1,
        }
    }
    false
}

/// `*` immediately after `::` (whitespace skipped) is `a::*`, not `{, *}`.
fn preceded_by_colon_colon(b: &[u8], star: usize) -> bool {
    let mut j = star;
    while j > 0 && b[j - 1].is_ascii_whitespace() {
        j -= 1;
    }
    j >= 2 && &b[j - 2..j] == b"::"
}

/// The `#[cfg(test)]` items' byte extents: the `#` through the item's `;`
/// or the `}` closing its `{` — whichever comes first at bracket depth 0.
/// `#[…]` attributes between the attribute and its item are skipped too.
/// When the attribute does not open an item (`opens_item` — a field,
/// variant, parameter or match arm, including a `pub`/`pub(…)`-qualified
/// field) only the attribute itself is masked — masking on would swallow
/// the item after it. A keyword-headed non-item position still goes
/// through `item_end`, which never crosses a `)`/`]`/`}` the item did
/// not open. `cfg(all(test, …))` is not recognised — none exist; a new
/// one needs a case here rather than an allow-list entry.
fn cfg_test_spans(text: &str) -> Vec<(usize, usize)> {
    let b = text.as_bytes();
    let mut spans = Vec::new();
    let mut i = 0;
    while i < b.len() {
        i = match b[i] {
            b'#' => match cfg_test_attr(b, i) {
                Some(end) => {
                    let start = item_start(b, end);
                    let close = if opens_item(text, start) {
                        item_end(b, start)
                    } else {
                        end // attribute on a non-item position: mask only it
                    };
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

/// Whether the word at `start` opens an item the attribute should mask
/// whole (`ITEM_KEYWORDS`), with one carve-out: `pub` is an item keyword
/// AND a field/variant visibility — `struct S { #[cfg(test)] pub x: u8 }`,
/// `struct T(#[cfg(test)] pub u8)`. For `pub`, the word after any `(…)`
/// visibility group (`pub(crate)`, `pub(in a::b)`) decides: an item
/// keyword opens `pub fn`/`pub(crate) mod`/…; anything else is a field
/// name or type, and the common `pub` case stays at attribute precision
/// instead of masking the field's text up to the enclosing closer
/// (rigor-rs#653).
fn opens_item(text: &str, start: usize) -> bool {
    let b = text.as_bytes();
    let first = ident_at(text, start);
    if first != "pub" {
        return ITEM_KEYWORDS.contains(&first);
    }
    let mut i = skip_ws(b, start + first.len());
    if b.get(i) == Some(&b'(') {
        i = close_of(b, i) + 1;
    }
    ITEM_KEYWORDS.contains(&ident_at(text, skip_ws(b, i)))
}

/// The first word after `#[cfg(test)]` (and any attributes it carries)
/// must open an item. On a non-item position — a struct field, enum
/// variant, parameter or match arm — only the attribute itself should
/// be masked; a keyword-headed one (a field or parameter named `union`,
/// a `fn()`-typed field) still goes through `item_end`, which never
/// crosses a `)`/`]`/`}` the item did not open (rigor-rs#653). `pub`
/// needs a look one word deeper (`opens_item`): it opens `pub fn`/
/// `pub(crate) mod`/… but also heads a visibility-qualified field.
const ITEM_KEYWORDS: &[&str] = &[
    "pub",
    "mod",
    "fn",
    "use",
    "impl",
    "const",
    "static",
    "struct",
    "enum",
    "type",
    "trait",
    "unsafe",
    "async",
    "extern",
    "union",
    "macro_rules",
    "macro",
];

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
/// (`mod x {…}`, `fn`, `impl`, a block expression). A `)`, `]` or `}` at
/// depth 0 is a closer the item never opened — it ends the parameter
/// list or block the position sits in — so the scan stops there too
/// rather than running past the enclosing item (rigor-rs#653). End of
/// file counts as the end.
fn item_end(b: &[u8], from: usize) -> usize {
    let mut depth = 0usize;
    for i in from..b.len() {
        match b[i] {
            b'(' | b'[' => depth += 1,
            b')' | b']' | b'}' if depth == 0 => return i,
            b')' | b']' => depth -= 1,
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

/// rigor-rs#653: `#[cfg(test)]` on a `pub`-qualified field masks only the
/// attribute — `pub` is itself an item keyword, so the whole-item extent
/// used to run past the struct's `}` to the next `{` and swallow that
/// item (hits inside it went unflagged). The same run-past held for any
/// keyword in a non-item position (a `union` parameter, a `fn()`-typed
/// field): the extent now never crosses a `)`/`]`/`}` the item did not
/// open.
#[test]
fn cfg_test_span_on_non_item_positions() {
    // (source, masked text). `pub` on an item masks the item; on a field,
    // variant or other non-item position only the attribute.
    for (src, want) in [
        // the issue's repro: the attribute masks only itself, so the
        // trailing item's `thread::spawn` stays visible to the audit
        (
            "struct S { #[cfg(test)] pub probe: u8, real: u8 }\nfn run() { thread::spawn(|| {}); }",
            "#[cfg(test)]",
        ),
        ("struct T(#[cfg(test)] pub u8);", "#[cfg(test)]"),
        ("struct T(#[cfg(test)] pub (u8, u8));", "#[cfg(test)]"),
        ("enum E { #[cfg(test)] pub V { x: u8 } }", "#[cfg(test)]"),
        (
            "struct S { #[cfg(test)] pub(crate) probe: u8 }",
            "#[cfg(test)]",
        ),
        (
            "#[cfg(test)] pub fn helper() {}",
            "#[cfg(test)] pub fn helper() {}",
        ),
        (
            "#[cfg(test)] pub(crate) async fn helper() {}",
            "#[cfg(test)] pub(crate) async fn helper() {}",
        ),
        (
            "#[cfg(test)] pub(in crate::a) mod m { fn f() {} }",
            "#[cfg(test)] pub(in crate::a) mod m { fn f() {} }",
        ),
        ("#[cfg(test)] pub use a::b;", "#[cfg(test)] pub use a::b;"),
        (
            "#[cfg(test)] pub static X: u8 = 1;",
            "#[cfg(test)] pub static X: u8 = 1;",
        ),
        // a keyword in a non-item position stops at the closer the
        // item never opened instead of running on to the next `{`
        (
            "struct S { #[cfg(test)] union: u8 } fn after() { thread::spawn(|| {}); }",
            "#[cfg(test)] union: u8 ",
        ),
        (
            "enum E { V(#[cfg(test)] fn(), u8), W } fn after() { thread::spawn(|| {}); }",
            "#[cfg(test)] fn(), u8",
        ),
        (
            "fn f(#[cfg(test)] union: u8, y: u8) { thread::spawn(|| {}); }",
            "#[cfg(test)] union: u8, y: u8",
        ),
        (
            "fn g() { foo(#[cfg(test)] union, thread::spawn(|| {})); }",
            "#[cfg(test)] union, thread::spawn(|| {})",
        ),
    ] {
        let spans = cfg_test_spans(&blank(src));
        assert_eq!(spans.len(), 1, "{src}");
        assert_eq!(&src[spans[0].0..spans[0].1], want, "{src}");
    }

    // End to end on the repro: the `thread::spawn` after the attribute is
    // a live hit, not masked inside a swollen `#[cfg(test)]` extent. The
    // weak-keyword parameter (`union` is a legal parameter name) stops at
    // the `)` the position never opened, so the body stays audited.
    for src in [
        "struct S { #[cfg(test)] pub probe: u8, real: u8 }\nfn run() { thread::spawn(|| {}); }",
        "fn f(#[cfg(test)] union: u8, y: u8) { thread::spawn(|| {}); }",
    ] {
        let text = blank(src);
        let spans = cfg_test_spans(&text);
        let active = |at: usize| !spans.iter().any(|&(s, e)| at >= s && at < e);
        assert_eq!(
            find_seq(&text, &["thread", "::", "spawn"], &active).len(),
            1,
            "{src}"
        );
    }
}

/// rigor-rs#652: a glob written with braces (`::{*}`, `::{a, *}`) is the
/// same smuggle as `::*`. rustfmt collapses `::{*}` to `::*`, but fmt is
/// not a gate, so both spellings — and mixed lists that still glob — must
/// flag. Nested child globs (`::{a::*}`, `::{a::{*}}`) are not a glob of
/// the parent ident.
#[test]
fn brace_glob_uses_flag_like_star_globs() {
    for (src, name, want) in [
        (" rigor_parse::*", "rigor_parse", true),
        (" rigor_parse:: *", "rigor_parse", true),
        (" rigor_parse :: *", "rigor_parse", true),
        (" rigor_parse::{*}", "rigor_parse", true),
        (" rigor_parse:: { * }", "rigor_parse", true),
        (" rigor_parse :: { * }", "rigor_parse", true),
        (" rigor_parse::{ * }", "rigor_parse", true),
        (" rigor_parse::{a, *}", "rigor_parse", true),
        (" rigor_parse::{*, a}", "rigor_parse", true),
        (" rigor_parse::{a, * , b}", "rigor_parse", true),
        (" std::thread::{*}", "thread", true),
        (" std::thread::{spawn, *}", "thread", true),
        (" std::{thread::{*}}", "thread", true),
        (" std::{thread::*}", "thread", true),
        (" rigor_parse::ruby_prism::{*}", "ruby_prism", true),
        (" rigor_parse::{ruby_prism::{*}}", "ruby_prism", true),
        (" rigor_parse::parse", "rigor_parse", false),
        (" rigor_parse::{parse}", "rigor_parse", false),
        (" rigor_parse::{a, parse}", "rigor_parse", false),
        (" rigor_parse::{a::*}", "rigor_parse", false),
        (" rigor_parse::{a::{*}}", "rigor_parse", false),
        (" rigor_parse::{a::b, c::*}", "rigor_parse", false),
        (" rigor_parse as rp", "rigor_parse", false),
    ] {
        let at = src.find(name).unwrap();
        assert_eq!(
            after_ident_is_glob(src, (at, &src[at..at + name.len()])),
            want,
            "{src}"
        );
    }

    let tags = |src: &str, internal: bool| {
        let text = blank(src);
        let uses = use_item_spans(&text);
        let mut tags: Vec<String> = flagged_uses(&text, &uses, internal)
            .into_iter()
            .map(|(_, tag)| tag)
            .collect();
        tags.sort();
        tags
    };
    for src in [
        "use rigor_parse::*;",
        "use rigor_parse::{*};",
        "use rigor_parse::{ * };",
        "use rigor_parse :: { * };",
        "use rigor_parse::{a, *};",
        "use rigor_parse::{*, a};",
    ] {
        assert!(
            tags(src, false).contains(&"use:alias-or-glob".to_string()),
            "{src}"
        );
    }
    for src in [
        "use std::thread::*;",
        "use std::thread::{*};",
        "use std::thread::{ * };",
        "use std::thread::{spawn, *};",
        "use std::thread::{*, Builder};",
        "use std::{thread::{*}};",
        "use std::{thread::*};",
    ] {
        let got = tags(src, false);
        assert!(got.contains(&"use:alias-or-glob".to_string()), "{src}: {got:?}");
    }
    // mixed `{spawn, *}` still names the leaf — keep the leaf tag too
    assert!(
        tags("use std::thread::{spawn, *};", false).contains(&"use:spawn".to_string())
    );
    for src in [
        "use rigor_parse::{parse};",
        "use rigor_parse::{a, parse};",
        "use rigor_parse::{a::*};",
        "use rigor_parse::{a::{*}};",
        "use std::thread::{spawn};",
        "use std::process::{*};",
    ] {
        assert!(
            !tags(src, false).contains(&"use:alias-or-glob".to_string()),
            "{src}"
        );
    }
    // the issue's planted shape: the glob flags, so a later bare `parse`/
    // `spawn` call is not an unguarded hole even though call-site patterns
    // cannot see the imported leaf.
    assert!(
        tags(
            "use rigor_parse::{*};\nfn f(src: &[u8]) { let _ = parse(src); }",
            false,
        )
        .contains(&"use:alias-or-glob".to_string())
    );
    assert!(
        tags(
            "use std::thread::{*};\nfn f() { spawn(|| {}); }",
            false,
        )
        .contains(&"use:alias-or-glob".to_string())
    );
}

/// rigor-rs#676: the pool-ordering findings of one `fn main`-bearing
/// file. A hit is a [`MAIN_POOL_TOKENS`] token inside the production
/// `fn main`'s body — called, taken as a value or built — or (rigor-rs#724)
/// a `use` item in that body whose tree reaches a pool token behind an
/// `as` alias or a glob ([`pool_use_hides_token`]); other `use` extents
/// are masked as "not a call". The production `fn main` count is pinned
/// at exactly one:
/// none or several is itself a finding, so a rename or a merge can never
/// let the check pass vacuously.
fn main_pool_findings(text: &str, rel: &str) -> Vec<String> {
    let b = text.as_bytes();
    let spans = cfg_test_spans(text);
    let no_test = |at: usize| !spans.iter().any(|&(s, e)| at >= s && at < e);
    let uses = use_item_spans(text);
    let mains = find_seq(text, &["fn", "main"], &no_test);
    let mut findings = Vec::new();
    let Some(&(at, _)) = mains.first() else {
        findings.push(format!(
            "{rel}: no production `fn main` — the pool-ordering audit (rigor-rs#676) \
             cannot run; update it with the move"
        ));
        return findings;
    };
    if mains.len() > 1 {
        let lines: Vec<String> = mains.iter().map(|&(a, _)| line_of(text, a).to_string()).collect();
        findings.push(format!(
            "{rel}: {} production `fn main`s (lines {}) — the pool-ordering audit \
             (rigor-rs#676) pins exactly one; merge them or update the audit",
            mains.len(),
            lines.join(", ")
        ));
        return findings;
    }
    let (open, close) = item_body(b, at);
    let active = |p: usize| (open..close).contains(&p) && !uses.iter().any(|&(s, e)| p >= s && p < e);
    for tok in MAIN_POOL_TOKENS {
        for (tok_at, _) in find_seq(text, &[tok], &active) {
            findings.push(format!(
                "{rel}:{}: `{tok}` in `fn main` — {MAIN_POOL_ADVICE}",
                line_of(text, tok_at)
            ));
        }
    }
    // rigor-rs#724: a `use` in the body whose tree reaches a pool token
    // behind an `as` alias or a glob renames the call site out of the
    // token matcher's reach — flag the use itself (its extent stays
    // masked above, so a token inside it does not also report).
    for &(u_at, u_end) in &uses {
        if !(open..close).contains(&u_at) || !no_test(u_at) {
            continue;
        }
        if pool_use_hides_token(&text[u_at + 3..u_end - 1]) {
            findings.push(format!(
                "{rel}:{}: `use` aliases or globs a pool token in `fn main` — \
                 {MAIN_POOL_ADVICE}",
                line_of(text, u_at)
            ));
        }
    }
    findings
}

/// Whether a `use` item's tree (the text between `use` and `;`) hides a
/// [`MAIN_POOL_TOKENS`] token from the `fn main` token match
/// (rigor-rs#724):
///
/// - the token leaf renamed — `use threads::configure_global_pool as
///   go`, `use rayon::{ThreadPoolBuilder as TPB}`, or the `self`
///   spelling `use threads::configure_global_pool::{self as go}`;
/// - a glob of the token's module — `use threads::*`, `use
///   threads::{run_command_thread, *}`, `use crate::threads::*`, `use
///   rayon::*`.
///
/// A plain `use threads::configure_global_pool` stays silent: the name
/// comes in unchanged, so any call still carries the token. An aliased
/// SEGMENT (`use threads as t`, `use threads::{self as t}`) stays silent
/// for the same reason — `t::configure_global_pool()` still names it.
fn pool_use_hides_token(item: &str) -> bool {
    let toks: Vec<(usize, &str)> = ident_spans(item).collect();
    let pair = |x: &str, y: &str| toks.windows(2).any(|w| w[0].1 == x && w[1].1 == y);
    if MAIN_POOL_TOKENS.iter().any(|&tok| pair(tok, "as")) {
        return true;
    }
    if toks.iter().any(|t| MAIN_POOL_TOKENS.contains(&t.1)) && pair("self", "as") {
        return true;
    }
    toks.iter()
        .any(|&t| POOL_USE_ROOTS.contains(&t.1) && after_ident_is_glob(item, t))
}

/// The `(open, close)` of the first depth-0 `{` at or after `at` — a
/// `fn main` hit's body — through its matching `}`. The parameter
/// list's `(`/`)` are crossed on the way, never mistaken for the body.
fn item_body(b: &[u8], at: usize) -> (usize, usize) {
    let mut depth = 0usize;
    for i in at..b.len() {
        match b[i] {
            b'(' | b'[' => depth += 1,
            b')' | b']' => depth = depth.saturating_sub(1),
            b'{' if depth == 0 => return (i, close_of(b, i)),
            _ => {}
        }
    }
    panic!("`fn main` at byte {at} opens no body")
}

/// rigor-rs#676: the #431 ordering — the command thread reserves its
/// stack BEFORE the rayon pool is built — rests on `fn main` routing
/// through `threads::run_command_thread(run_command_with_pool)` and
/// never touching the pool itself. The threads unit test
/// `command_thread_is_spawned_before_the_command_body` pins only
/// `run_command_thread_with`'s own spawn-before-body sequence and would
/// still pass if `main` went back to configuring the pool directly, so
/// this source check is the pin.
#[test]
fn main_never_builds_the_pool_itself() {
    let path = workspace_root().join("crates/rigor-cli/src/main.rs");
    let raw = fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path:?}: {e}"));
    let findings = main_pool_findings(&blank(&raw), "rigor-cli/src/main.rs");
    assert!(
        findings.is_empty(),
        "pool-ordering audit (rigor-rs#676, #431): `fn main` must not build the rayon pool:\n{}",
        findings.join("\n")
    );
}

/// rigor-rs#676: `main_pool_findings` flags direct configuration only —
/// the current `main` shape stays clean, every inversion shape (a call,
/// a function value, a bare or stack-proven builder, a spaced qualifier)
/// flags, and the boundaries hold: a call after `fn main`'s `}` is the
/// command body's business, and a method call, an identifier that merely
/// contains or abuts the token, a `use` inside the body and a
/// `#[cfg(test)]` sibling are not hits. Zero or several production
/// `fn main`s are findings, so the check cannot pass vacuously.
#[test]
fn main_pool_findings_flag_direct_configuration_only() {
    for (src, want) in [
        // the current shape: routing through the command thread only,
        // the pool built in the command body (after main's `}`)
        (
            "fn main() -> ExitCode { threads::run_command_thread(run_command_with_pool) } \
             fn run_command_with_pool() -> ExitCode { \
             threads::configure_global_pool(); dispatch(&args) }",
            Vec::new(),
        ),
        // the #431 inversion: the call moved back into main
        (
            "fn main() -> ExitCode { if !threads::configure_global_pool() { \
             return code() } run(body) }",
            vec!["`configure_global_pool` in `fn main`"],
        ),
        // a stack-proven builder in main — the repo-wide pass accepts
        // this chain; in `fn main` it is still the inversion
        (
            "fn main() { rayon::ThreadPoolBuilder::new() \
             .spawn_handler(spawn_pool_thread).build_global(); run() }",
            vec!["`ThreadPoolBuilder` in `fn main`"],
        ),
        // `::default()` and a bare unqualified builder name
        (
            "fn main() { let pool = ThreadPoolBuilder::default().build_global(); }",
            vec!["`ThreadPoolBuilder` in `fn main`"],
        ),
        // whitespace around `::` and before `(` cannot bypass the match
        (
            "fn main () { threads :: configure_global_pool ( ) }",
            vec!["`configure_global_pool` in `fn main`"],
        ),
        // a function value is the same hazard as a call
        (
            "fn main() { let build = threads::configure_global_pool; }",
            vec!["`configure_global_pool` in `fn main`"],
        ),
        // a method call is not the free function
        ("fn main() { pool.configure_global_pool(); }", Vec::new()),
        // a longer — or abutting — identifier is not the token
        ("fn main() { configure_global_pool_twice(); }", Vec::new()),
        ("fn main() { xconfigure_global_pool(); }", Vec::new()),
        // a `use` inside the body is not a call; the call after it is
        (
            "fn main() { use threads::configure_global_pool; run(body) }",
            Vec::new(),
        ),
        (
            "fn main() { use threads::configure_global_pool; configure_global_pool() }",
            vec!["`configure_global_pool` in `fn main`"],
        ),
        // rigor-rs#724: the alias renames the call site out of the
        // token matcher's reach — flag the `use` itself
        (
            "fn main() -> ExitCode { use threads::configure_global_pool as go; \
             if !go() { return code() } threads::run_command_thread(body) }",
            vec!["`use` aliases or globs a pool token in `fn main`"],
        ),
        (
            "fn main() { use rayon::ThreadPoolBuilder as TPB; TPB::new().build_global(); }",
            vec!["`use` aliases or globs a pool token in `fn main`"],
        ),
        (
            "fn main() { use rayon::{ThreadPoolBuilder as TPB}; TPB::new(); }",
            vec!["`use` aliases or globs a pool token in `fn main`"],
        ),
        (
            "fn main() { use threads::configure_global_pool::{self as go}; go() }",
            vec!["`use` aliases or globs a pool token in `fn main`"],
        ),
        // a glob of the token's module is the same indirection, even
        // though a bare call would still carry the token
        (
            "fn main() { use threads::*; run(body) }",
            vec!["`use` aliases or globs a pool token in `fn main`"],
        ),
        (
            "fn main() { use threads::{run_command_thread, *}; run(body) }",
            vec!["`use` aliases or globs a pool token in `fn main`"],
        ),
        (
            "fn main() { use crate::threads::*; run() }",
            vec!["`use` aliases or globs a pool token in `fn main`"],
        ),
        (
            "fn main() { use rayon::*; run() }",
            vec!["`use` aliases or globs a pool token in `fn main`"],
        ),
        // an aliased use AND a bare call are two findings
        (
            "fn main() { use threads::configure_global_pool as go; configure_global_pool() }",
            vec![
                "`use` aliases or globs a pool token in `fn main`",
                "`configure_global_pool` in `fn main`",
            ],
        ),
        // an aliased SEGMENT does not hide the token — the qualified
        // call still names it and flags on its own
        (
            "fn main() { use threads as t; t::configure_global_pool() }",
            vec!["`configure_global_pool` in `fn main`"],
        ),
        (
            "fn main() { use threads::{self as t}; t::configure_global_pool() }",
            vec!["`configure_global_pool` in `fn main`"],
        ),
        // a plain use of the token — braced or not — stays silent (the
        // call flags itself), and a use outside the body is not its
        // business
        (
            "fn main() { use threads::{configure_global_pool}; run(body) }",
            Vec::new(),
        ),
        (
            "use threads::configure_global_pool as go; fn main() { run() }",
            Vec::new(),
        ),
        // a `#[cfg(test)]` sibling `fn main` is masked; the production
        // one stays clean
        (
            "#[cfg(test)] mod t { fn main() { configure_global_pool() } } \
             fn main() { run() }",
            Vec::new(),
        ),
        // zero or several production `fn main`s never pass vacuously
        (
            "fn cli() { threads::configure_global_pool() }",
            vec!["no production `fn main`"],
        ),
        (
            "fn main() { a() } fn main() { b() }",
            vec!["2 production `fn main`s"],
        ),
    ] {
        let findings = main_pool_findings(&blank(src), "main.rs");
        assert_eq!(findings.len(), want.len(), "{src}\n{findings:?}");
        for w in &want {
            assert!(findings.iter().any(|f| f.contains(w)), "{src}\n{findings:?}");
        }
    }
}
