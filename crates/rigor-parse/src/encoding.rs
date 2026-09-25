//! The script encoding Prism resolves for a source, reduced to the one bit
//! [`LoweredAst::utf8_source`](crate::ast::LoweredAst::utf8_source) needs:
//! whether that encoding IS UTF-8.
//!
//! This is a faithful port of the three pieces of vendored prism (1.9.0) that
//! decide it — the `ruby-prism` binding does not expose `parser->encoding`, so
//! the resolution is re-derived byte-for-byte from `src/prism.c`:
//!
//!   * the `pm_parser_init` plumbing that computes `encoding_comment_start`
//!     (the BOM skip, the `#!`-shebang handling — line 2 is honored ONLY when
//!     the shebang contains `ruby`, and the inline-whitespace advance);
//!   * [`magic_comment_pass`] — `parser_lex_magic_comment`, the `key: value`
//!     scanner with `-*-` emacs markers and `;` separators, whose return value
//!     also decides whether the fallback runs;
//!   * [`fallback_coding_scan`] — `parser_lex_magic_comment_encoding`, the loose
//!     `coding` substring scan that honors `vim: set fileencoding=…` modelines,
//!     free-scan `coding:` comments, `encoding = binary` (spaced `=`), and the
//!     same comment's trailing tokens;
//!   * [`find_encoding`] — `pm_encoding_find` collapsed to
//!     utf-8 / non-utf-8 / unresolved. The full name table is compiled in (the
//!     `ruby-prism-sys` build defines no `PRISM_ENCODING_EXCLUDE_FULL`), so the
//!     name list below mirrors the whole switch.
//!
//! Every comparison is `pm_strncasecmp`-equivalent ASCII-insensitive, every
//! scan is over raw bytes, and an unresolved name keeps the previous encoding
//! (UTF-8 initially) with a parse error — exactly like the C.

/// `pm_char_is_whitespace`: `\t`, `\n`, `\v`, `\f`, `\r`, ` `.
fn is_ws(b: u8) -> bool {
    matches!(b, b'\t' | b'\n' | 0x0B | 0x0C | b'\r' | b' ')
}

/// `pm_strspn_inline_whitespace` bytes: inline whitespace, no `\n`.
fn is_inline_ws(b: u8) -> bool {
    matches!(b, b'\t' | 0x0B | 0x0C | b'\r' | b' ')
}

/// `pm_char_is_magic_comment_key_delimiter`: `'`, `"`, `:`, `;`.
fn is_key_delim(b: u8) -> bool {
    matches!(b, b'\'' | b'"' | b':' | b';')
}

/// `parser_lex_magic_comment_emacs_marker`: the next `-*-` at or after
/// `cursor`, its `from` bounded by `to` (both indexes into `buf`).
fn find_marker(buf: &[u8], from: usize, to: usize) -> Option<usize> {
    let mut cursor = from;
    while cursor + 3 <= to {
        match buf[cursor..to].iter().position(|&b| b == b'-') {
            Some(i) => {
                cursor += i;
                if cursor + 3 <= to && buf[cursor + 1] == b'*' && buf[cursor + 2] == b'-' {
                    return Some(cursor);
                }
                cursor += 1;
            }
            None => return None,
        }
    }
    None
}

/// `parser_lex_magic_comment` on the comment token `comment` (which INCLUDES
/// the leading `#`; `comment.len()` is `current.end - current.start`, newline
/// included when the line has one). Applies every `encoding`/`coding` key to
/// `resolved` in order — the last one wins — and returns the C `result`: whether
/// the comment was consumed entirely, EXCEPT that a failed encoding-value apply
/// flips it to `false`, which is what lets the fallback run afterwards.
///
/// The C swaps `-` for `_` inside the key before comparing; that transform can
/// never turn a different key INTO `encoding`/`coding` (neither contains `_`
/// or `-`), so the raw key slice is compared directly.
fn magic_comment_pass(comment: &[u8], resolved: &mut bool) -> bool {
    let mut start = 1usize; // current.start + 1
    let mut end = comment.len();
    if end - start <= 7 {
        return false;
    }

    let mut indicator = false;
    if let Some(m) = find_marker(comment, start, end) {
        start = m + 3;
        match find_marker(comment, start, end) {
            Some(m2) => {
                end = m2;
                indicator = true;
            }
            // A start marker without an end marker cannot be a magic comment.
            None => return false,
        }
    }

    let mut result = true;
    let mut cursor = start;
    while cursor < end {
        while cursor < end && (is_key_delim(comment[cursor]) || is_ws(comment[cursor])) {
            cursor += 1;
        }
        let key_start = cursor;
        while cursor < end && !(is_key_delim(comment[cursor]) || is_ws(comment[cursor])) {
            cursor += 1;
        }
        let key_end = cursor;
        while cursor < end && is_ws(comment[cursor]) {
            cursor += 1;
        }
        if cursor == end {
            break;
        }

        if comment[cursor] == b':' {
            cursor += 1;
        } else if !indicator {
            return false;
        } else {
            continue;
        }

        while cursor < end && is_ws(comment[cursor]) {
            cursor += 1;
        }
        if cursor == end {
            break;
        }

        let (value_start, value_end);
        if comment[cursor] == b'"' {
            cursor += 1;
            value_start = cursor;
            while cursor < end && comment[cursor] != b'"' {
                if comment[cursor] == b'\\' && cursor + 1 < end {
                    cursor += 1;
                }
                cursor += 1;
            }
            value_end = cursor;
            if cursor < end && comment[cursor] == b'"' {
                cursor += 1;
            }
        } else {
            value_start = cursor;
            while cursor < end
                && comment[cursor] != b'"'
                && comment[cursor] != b';'
                && !is_ws(comment[cursor])
            {
                cursor += 1;
            }
            value_end = cursor;
        }

        if indicator {
            while cursor < end && (comment[cursor] == b';' || is_ws(comment[cursor])) {
                cursor += 1;
            }
        } else {
            while cursor < end && is_ws(comment[cursor]) {
                cursor += 1;
            }
            if cursor != end {
                return false;
            }
        }

        let key = &comment[key_start..key_end];
        if (key.len() == 8 && key.eq_ignore_ascii_case(b"encoding"))
            || (key.len() == 6 && key.eq_ignore_ascii_case(b"coding"))
        {
            result = apply_encoding(resolved, &comment[value_start..value_end]);
        }
    }
    result
}

/// `parser_lex_magic_comment_encoding` — the loose fallback scan for a
/// `coding`-shaped key anywhere in the comment (`comment` includes `#`).
/// Called only when [`magic_comment_pass`] did not consume the comment, the
/// comment is at least 10 bytes, and it sits at `encoding_comment_start`.
fn fallback_coding_scan(comment: &[u8], resolved: &mut bool) {
    let end = comment.len();
    let mut cursor = 1usize; // current.start + 1
    let mut separator = false;

    // Find the first `coding` substring that is followed (after optional
    // whitespace) by `=` or `:`. The switch is the C skip-ahead: `cursor[6]`
    // is the byte just past a six-byte `coding` candidate.
    loop {
        if end - cursor <= 6 {
            return;
        }
        match comment[cursor + 6] {
            b'C' | b'c' => {
                cursor += 6;
                continue;
            }
            b'O' | b'o' => {
                cursor += 5;
                continue;
            }
            b'D' | b'd' => {
                cursor += 4;
                continue;
            }
            b'I' | b'i' => {
                cursor += 3;
                continue;
            }
            b'N' | b'n' => {
                cursor += 2;
                continue;
            }
            b'G' | b'g' => {
                cursor += 1;
                continue;
            }
            b'=' | b':' => {
                separator = true;
                cursor += 6;
            }
            _ => {
                cursor += 6;
                if !is_ws(comment[cursor]) {
                    continue;
                }
            }
        }
        if comment[cursor - 6..cursor].eq_ignore_ascii_case(b"coding") {
            break;
        }
        separator = false;
    }

    // Skip to the `=`/`:` separator (it may be whitespace-separated from the
    // `coding` key), then to the value.
    loop {
        loop {
            cursor += 1;
            if cursor >= end {
                return;
            }
            if !is_ws(comment[cursor]) {
                break;
            }
        }
        if separator {
            break;
        }
        if comment[cursor] != b'=' && comment[cursor] != b':' {
            return;
        }
        separator = true;
        cursor += 1;
    }

    let value_start = cursor;
    // `parser->encoding->alnum_char(cursor, 1)` on the still-UTF-8 encoding: a
    // lone ≥0x80 byte cannot complete a multibyte character, so it is never
    // alphanumeric here — `[0-9A-Za-z]` plus `-`/`_` is the full set.
    while cursor < end
        && (comment[cursor] == b'-' || comment[cursor] == b'_'
            || comment[cursor].is_ascii_alphanumeric())
    {
        cursor += 1;
    }
    apply_encoding(resolved, &comment[value_start..cursor]);
}

/// `parser_lex_magic_comment_encoding_value`: apply a scanned encoding name.
/// Returns the C function's `bool` — `false` on an unresolved name, which also
/// leaves `resolved` untouched (the parser keeps the previous encoding and
/// emits `PM_ERR_INVALID_ENCODING_MAGIC_COMMENT`).
fn apply_encoding(resolved: &mut bool, value: &[u8]) -> bool {
    match find_encoding(value) {
        Some(utf8) => {
            *resolved = utf8;
            true
        }
        None => false,
    }
}

/// `pm_encoding_find` collapsed: `Some(true)` when the name resolves to
/// `PM_ENCODING_UTF_8_ENTRY`, `Some(false)` when it resolves to any other
/// entry, `None` when it resolves to none. Widths are exact and the match is
/// ASCII case-insensitive, as in the C switch.
fn find_encoding(value: &[u8]) -> Option<bool> {
    // UTF-8 first, like the C: any `UTF-8*` name of 5+ bytes is UTF-8 except
    // `UTF-8-HFS`, which selects the UTF8-MAC entry.
    if value.len() >= 5 && value[..5].eq_ignore_ascii_case(b"utf-8") {
        if value.len() == 9 && value[5..].eq_ignore_ascii_case(b"-hfs") {
            return Some(false);
        }
        return Some(true);
    }
    if value.len() < 3 {
        return None;
    }
    if value.eq_ignore_ascii_case(b"cp65001") {
        return Some(true);
    }
    NON_UTF8_NAMES
        .iter()
        .any(|name| value.eq_ignore_ascii_case(name))
        .then_some(false)
}

/// Every `pm_encoding_find` table name that resolves to a non-UTF-8 entry
/// (the `ruby-prism-sys` build compiles the full table). `CP65001` is the only
/// non-`UTF-8*` name resolving to the UTF-8 entry and is handled above.
static NON_UTF8_NAMES: &[&[u8]] = &[
    b"ASCII",
    b"ASCII-8BIT",
    b"ANSI_X3.4-1968",
    b"BINARY",
    b"Big5",
    b"Big5-HKSCS",
    b"Big5-HKSCS:2008",
    b"Big5-UAO",
    b"CP932",
    b"csWindows31J",
    b"CESU-8",
    b"CP437",
    b"CP720",
    b"CP737",
    b"CP775",
    b"CP850",
    b"CP852",
    b"CP855",
    b"CP857",
    b"CP860",
    b"CP861",
    b"CP862",
    b"CP864",
    b"CP865",
    b"CP866",
    b"CP869",
    b"CP874",
    b"CP878",
    b"CP863",
    b"CP936",
    b"CP949",
    b"CP950",
    b"CP951",
    b"CP1250",
    b"CP1251",
    b"CP1252",
    b"CP1253",
    b"CP1254",
    b"CP1255",
    b"CP1256",
    b"CP1257",
    b"CP1258",
    b"CP51932",
    b"EUC-JP",
    b"eucJP",
    b"eucJP-ms",
    b"euc-jp-ms",
    b"EUC-JIS-2004",
    b"EUC-JISX0213",
    b"EUC-KR",
    b"eucKR",
    b"EUC-CN",
    b"eucCN",
    b"EUC-TW",
    b"eucTW",
    b"Emacs-Mule",
    b"GBK",
    b"GB12345",
    b"GB18030",
    b"GB1988",
    b"GB2312",
    b"IBM437",
    b"IBM720",
    b"IBM737",
    b"IBM775",
    b"IBM850",
    b"IBM852",
    b"IBM855",
    b"IBM857",
    b"IBM860",
    b"IBM861",
    b"IBM862",
    b"IBM863",
    b"IBM864",
    b"IBM865",
    b"IBM866",
    b"IBM869",
    b"ISO-8859-1",
    b"ISO8859-1",
    b"ISO-8859-2",
    b"ISO8859-2",
    b"ISO-8859-3",
    b"ISO8859-3",
    b"ISO-8859-4",
    b"ISO8859-4",
    b"ISO-8859-5",
    b"ISO8859-5",
    b"ISO-8859-6",
    b"ISO8859-6",
    b"ISO-8859-7",
    b"ISO8859-7",
    b"ISO-8859-8",
    b"ISO8859-8",
    b"ISO-8859-9",
    b"ISO8859-9",
    b"ISO-8859-10",
    b"ISO8859-10",
    b"ISO-8859-11",
    b"ISO8859-11",
    b"ISO-8859-13",
    b"ISO8859-13",
    b"ISO-8859-14",
    b"ISO8859-14",
    b"ISO-8859-15",
    b"ISO8859-15",
    b"ISO-8859-16",
    b"ISO8859-16",
    b"KOI8-R",
    b"KOI8-U",
    b"macCentEuro",
    b"macCroatian",
    b"macCyrillic",
    b"macGreek",
    b"macIceland",
    b"MacJapanese",
    b"MacJapan",
    b"macRoman",
    b"macRomania",
    b"macThai",
    b"macTurkish",
    b"macUkraine",
    b"PCK",
    b"SJIS",
    b"Shift_JIS",
    b"SJIS-DoCoMo",
    b"SJIS-KDDI",
    b"SJIS-SoftBank",
    b"stateless-ISO-2022-JP",
    b"stateless-ISO-2022-JP-KDDI",
    b"TIS-620",
    b"US-ASCII",
    b"UTF8-MAC",
    b"UTF8-DoCoMo",
    b"UTF8-KDDI",
    b"UTF8-SoftBank",
    b"Windows-31J",
    b"Windows-874",
    b"Windows-1250",
    b"Windows-1251",
    b"Windows-1252",
    b"Windows-1253",
    b"Windows-1254",
    b"Windows-1255",
    b"Windows-1256",
    b"Windows-1257",
    b"Windows-1258",
    b"646",
];

/// Whether the script encoding Prism resolves for `source` is UTF-8 — `true`
/// for no magic encoding comment (the default), for an explicit UTF-8 name
/// (`utf-8`, `CP65001`, `utf-8-unix`, …), and for an unresolved name (which
/// keeps the default). `false` for any other resolved name.
///
/// Mirrors `pm_parser_init` + the two magic-comment passes: the encoding
/// comment is honored ONLY at `encoding_comment_start` — line 1 (after a BOM
/// and leading inline whitespace), or line 2 when line 1 is a `#!` line that
/// contains `ruby`. A shebang WITHOUT `ruby` (`#!/bin/sh`, `#!/usr/bin/env
/// perl`) does not unlock line 2; Prism runs no shebang search outside
/// `main_script`/`-x` (the `ruby` CLI then reports "no Ruby script found",
/// but a library file simply keeps UTF-8 — probed).
pub(crate) fn resolved_utf8(source: &[u8]) -> bool {
    let mut comment_start = 0usize;
    // UTF-8 BOM skip.
    if source.len() >= 3 && source[..3] == [0xEF, 0xBB, 0xBF] {
        comment_start = 3;
    }
    // A `#!` first line containing `ruby` moves the encoding comment start to
    // line 2; any other content leaves it at line 1.
    let line1_nl = source[comment_start..]
        .iter()
        .position(|&b| b == b'\n')
        .map(|i| comment_start + i);
    let line1_end = line1_nl.unwrap_or(source.len());
    let line1 = &source[comment_start..line1_end];
    // `pm_strnstr(…, "ruby", …)` — a case-SENSITIVE substring search.
    if line1.len() > 2
        && line1[0] == b'#'
        && line1[1] == b'!'
        && line1.windows(4).any(|w| w == b"ruby")
        && line1_nl.is_some()
    {
        comment_start = line1_end + 1;
    }
    // The comment may begin after inline whitespace only.
    while comment_start < source.len() && is_inline_ws(source[comment_start]) {
        comment_start += 1;
    }
    let mut resolved = true;
    if comment_start >= source.len() || source[comment_start] != b'#' {
        return resolved;
    }
    // The comment token runs to just past the next `\n` (or EOF).
    let comment_end = match source[comment_start + 1..]
        .iter()
        .position(|&b| b == b'\n')
    {
        Some(i) => comment_start + 1 + i + 1,
        None => source.len(),
    };
    let comment = &source[comment_start..comment_end];
    if !magic_comment_pass(comment, &mut resolved) && comment.len() >= 10 {
        fallback_coding_scan(comment, &mut resolved);
    }
    resolved
}
