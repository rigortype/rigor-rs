//! The static half of `Builder::lower_node`'s disjointness contract (#418).
//! The debug check (`check_families_disjoint`) sees only the Prism types a
//! debug run lowers. This test reads the family files as text, so it covers
//! every claimed type: none may be claimed twice, in one family or two.
//!
//! # The layout it parses
//!
//! A family method's body is a flat run of top-level statements, ending in
//! `UNMATCHED`:
//!
//! - an arm, `if COND { … }`, with no `else`. COND is one of:
//!   - `let Some(..) = node.as_x_node()`;
//!   - `node.as_x_node().is_some()`, alone or joined by `||` (a term may
//!     also be `pred(node)`, a free function whose body is such a chain,
//!     like `is_unmodeled_write` in `builder.rs`);
//!   - `let Some(..) = name`, for a `name` bound by a top-level `let`;
//! - `let name = node.as_x_node().map(..)`, then any number of
//!   `.or_else(|| node.as_y_node().map(..))` links (`calls.rs`).
//!
//! An `if let` pattern is exactly `Some(..)`: `if let None = node.as_x_node()`
//! would claim every type but X.
//!
//! One arm per type; every arm ends in `return`. A second arm for a type is
//! dead code, so the test fails on it, even within one family. That includes
//! a fall-through arm followed by a second arm for its type, by design.
//! `lower_node` itself claims no type: its body holds no `as_x_node()` call.
//!
//! Only COND and the `node.as_x_node()` heads of a `let` chain count. Arm
//! bodies, `.map` closures, comments and literals are skipped. Any other
//! top-level statement fails the test: a new arm shape needs a case here.

use std::collections::BTreeMap;

/// `lower_node`'s families in try order: method, file, source, the locked
/// number of Prism types it claims, and a floor on its arm count. The floor
/// fails an extraction that silently finds nothing.
const FAMILY_FILES: [(&str, &str, &str, usize, usize); 5] = [
    ("lower_calls", "calls.rs", include_str!("calls.rs"), 7, 3),
    ("lower_expressions", "expressions.rs", include_str!("expressions.rs"), 35, 35),
    ("lower_control", "control.rs", include_str!("control.rs"), 13, 13),
    ("lower_definitions", "definitions.rs", include_str!("definitions.rs"), 4, 4),
    ("lower_jumps", "jumps.rs", include_str!("jumps.rs"), 31, 9),
];

/// `builder.rs`: `lower_node` itself and the arm predicate helpers.
const BUILDER: &str = include_str!("../builder.rs");

#[test]
fn lower_node_families_claim_disjoint_prism_types() {
    // The files read here are exactly `lower_node`'s families, in order.
    let mut dispatch: Vec<&str> = FAMILY_FILES.iter().map(|f| f.0).collect();
    dispatch.push("lower_fallback");
    assert_eq!(
        dispatched_families(),
        dispatch,
        "lower_node's family calls changed: update FAMILY_FILES"
    );

    let mut owner: BTreeMap<String, String> = BTreeMap::new();
    let mut twice = Vec::new();
    let mut claimed = Vec::new();
    for (method, file, src, _, _) in FAMILY_FILES {
        let arms = family_arms(method, file, src);
        let mut types = Vec::new();
        for (line, arm_types) in &arms {
            for ty in arm_types {
                let here = format!("{method} ({file}:{line})");
                if let Some(first) = owner.insert(ty.clone(), here.clone()) {
                    twice.push(format!("{ty} is claimed by both {first} and {here}"));
                }
                types.push(ty.clone());
            }
        }
        claimed.push((method, arms.len(), types));
    }
    assert!(
        twice.is_empty(),
        "lower_node's families must claim pairwise distinct Prism types:\n{}",
        twice.join("\n")
    );

    // No lock on the total: with the types distinct, it is the sum of
    // FAMILY_FILES's counts.
    for ((method, arms, types), (_, file, _, want_types, min_arms)) in
        claimed.iter().zip(FAMILY_FILES)
    {
        assert!(
            *arms >= min_arms,
            "{method}: {arms} arms, under the floor of {min_arms}: if arms were merged, lower \
             {method}'s floor in FAMILY_FILES; if not, the extraction found too few"
        );
        assert_eq!(
            types.len(),
            want_types,
            "{method} ({file}) claims {} Prism types, not {want_types}: update {method}'s \
             count in FAMILY_FILES. It claims {types:?}",
            types.len()
        );
    }
}

/// `blank` on literal prefixes (#420): `br` and `cr` start raw strings,
/// `b` and `c` escaped ones, and none of them starts a literal from inside
/// an identifier. Byte chars and lifetimes keep working.
#[test]
fn blank_reads_literal_prefixes() {
    for (src, want) in [
        (r#"br"\" + "\"{""#, r#"br" " + "   ""#),
        (r#"(cr"\", "\"{")"#, r#"(cr" ", "   ")"#),
        (r##"br#"say "{" hi"#;"##, r##"br#"          "#;"##),
        (r##"cr#"say "{" hi"#;"##, r##"cr#"          "#;"##),
        (r#"b"\"{" + c"\"{""#, r#"b"   " + c"   ""#),
        (r#"xbr"\"{" _cr"\"{" bar"\"{" sub"\"{""#, r#"xbr"   " _cr"   " bar"   " sub"   ""#),
        (r#"[b'"', b'\'', b'{'] &'b x"#, r#"[b' ', b'  ', b' '] &'b x"#),
        (r#"'br: loop { br"\" }"#, r#"'br: loop { br" " }"#),
        (r#"r#match + r"\" + "{""#, r#"r#match + r" " + " ""#),
    ] {
        assert_eq!(blank(src), want, "blank({src:?})");
    }
}

/// The `self.lower_*` methods `lower_node`'s body calls, in order. The body
/// may hold no `as_x_node()` call: a claim there escapes this test and the
/// debug check alike.
fn dispatched_families() -> Vec<&'static str> {
    let text = blank(BUILDER);
    let (open, close) = fn_body(&text, "lower_node", "builder.rs");
    for (at, _) in text[open..close].match_indices("as_") {
        let at = open + at;
        let name = ident_at(&text, at);
        let downcast = name.strip_prefix("as_").and_then(|s| s.strip_suffix("_node")).is_some();
        let line = text[..at].matches('\n').count() + 1;
        assert!(
            is_ident_byte(text.as_bytes()[at - 1]) || !downcast,
            "builder.rs:{line}: lower_node calls `{name}` itself: a type's arm goes in a family"
        );
    }
    let body = &BUILDER[open..close];
    let mut called: Vec<&str> = Vec::new();
    for (at, _) in text[open..close].match_indices("self.lower_") {
        let name = ident_at(body, at + "self.".len());
        if !called.contains(&name) {
            called.push(name);
        }
    }
    called
}

/// Each top-level arm of family `method` as (line, the Prism types it claims).
fn family_arms(method: &str, file: &str, src: &str) -> Vec<(usize, Vec<String>)> {
    let text = blank(src);
    let b = text.as_bytes();
    let (open, close) = fn_body(&text, method, file);
    let mut lets: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut arms = Vec::new();
    let mut ended = false;
    let mut i = open + 1;
    loop {
        while i < close && b[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= close {
            break;
        }
        let line = text[..i].matches('\n').count() + 1;
        let at = format!("{file}:{line}");
        assert!(!ended, "{at}: `UNMATCHED` must be {method}'s last expression");
        match ident_at(&text, i) {
            "if" => {
                let brace = find_top(b, i, b"{").filter(|&j| j < close);
                let brace = brace.unwrap_or_else(|| panic!("{at}: no arm body"));
                let cond = &text[i + "if".len()..brace];
                i = close_of(b, brace) + 1;
                let next = text[i..close].trim_start();
                assert!(ident_at(next, 0) != "else", "{at}: an arm takes no `else`");
                arms.push((line, cond_types(cond, &mut lets, &at)));
            }
            "let" => {
                let semi = find_top(b, i, b";").filter(|&j| j < close);
                let semi = semi.unwrap_or_else(|| panic!("{at}: unterminated `let`"));
                let stmt = &text[i + "let".len()..semi];
                let eq = find_top(stmt.as_bytes(), 0, b"=");
                let eq = eq.unwrap_or_else(|| panic!("{at}: `let` without `=`"));
                let name = stmt[..eq].trim();
                assert!(is_ident(name), "{at}: a top-level `let` binds one plain name");
                let types = chain_types(&stmt[eq + 1..], &at);
                assert!(lets.insert(name.to_string(), types).is_none(), "{at}: `{name}` rebound");
                i = semi + 1;
            }
            "UNMATCHED" => {
                ended = true;
                i += "UNMATCHED".len();
            }
            _ => panic!(
                "{at}: {method}'s top level holds only arms, `let` chains and the trailing \
                 `UNMATCHED` (see family_inventory.rs's module doc), not: {}",
                text[i..].lines().next().unwrap_or_default()
            ),
        }
    }
    assert!(ended, "{file}: {method} must end in `UNMATCHED`");
    assert!(lets.is_empty(), "{file}: no arm tests the `let` chain(s) {:?}", lets.keys());
    arms
}

/// The Prism types an arm's `if` condition tests.
fn cond_types(cond: &str, lets: &mut BTreeMap<String, Vec<String>>, at: &str) -> Vec<String> {
    let cond = cond.trim();
    if ident_at(cond, 0) != "let" {
        return split_top(cond, "||").iter().flat_map(|t| term_types(t, at)).collect();
    }
    let eq = find_top(cond.as_bytes(), 0, b"=").unwrap_or_else(|| panic!("{at}: `let` without `=`"));
    let pat = cond["let".len()..eq].trim();
    let some = pat.strip_prefix("Some").map(str::trim_start).filter(|p| p.starts_with('('));
    assert!(
        some.is_some_and(|p| close_of(p.as_bytes(), 0) == p.len() - 1),
        "{at}: an `if let` pattern is `Some(..)`, not `{pat}`"
    );
    let scrutinee = cond[eq + 1..].trim();
    if let Some((ty, rest)) = as_node_call(scrutinee) {
        assert!(rest.trim().is_empty(), "{at}: `if let` tests one `node.as_x_node()` only");
        return vec![ty];
    }
    lets.remove(scrutinee)
        .unwrap_or_else(|| panic!("{at}: `if let` on neither `node.as_x_node()` nor a `let` chain"))
}

/// One `||` term: `node.as_x_node().is_some()`, or `pred(node)` for a
/// helper whose body is a chain of such terms.
fn term_types(term: &str, at: &str) -> Vec<String> {
    let term = term.trim();
    if let Some((ty, rest)) = as_node_call(term) {
        let rest: String = rest.split_whitespace().collect();
        assert_eq!(rest, ".is_some()", "{at}: a `||` term is `node.as_x_node().is_some()`");
        return vec![ty];
    }
    let compact: String = term.split_whitespace().collect();
    let pred = compact.strip_suffix("(node)").filter(|p| is_ident(p));
    let pred = pred.unwrap_or_else(|| panic!("{at}: not an arm predicate: `{term}`"));
    let found: Vec<(&str, String)> = FAMILY_FILES
        .iter()
        .map(|f| (f.1, f.2))
        .chain([("builder.rs", BUILDER)])
        .map(|(file, src)| (file, blank(src)))
        .filter(|(_, text)| text.contains(&format!("fn {pred}(")))
        .collect();
    assert_eq!(found.len(), 1, "{at}: `{pred}` must be defined once");
    let (file, text) = &found[0];
    let (open, close) = fn_body(text, pred, file);
    let sig: String = text[..open].rsplit("fn ").next().unwrap_or_default().split_whitespace().collect();
    assert_eq!(sig, format!("{pred}(node:&PrismNode<'_>)->bool"), "{file}: `{pred}`'s signature");
    let at = format!("{file}: `{pred}`");
    split_top(&text[open + 1..close], "||").iter().flat_map(|t| term_types(t, &at)).collect()
}

/// The types a top-level `let` chain tests: its `node.as_x_node()` head and
/// the head of every `.or_else` link. `.map` closures are the arm's
/// bindings, not predicates, so they are skipped whole.
fn chain_types(expr: &str, at: &str) -> Vec<String> {
    let (ty, mut rest) = as_node_call(expr)
        .unwrap_or_else(|| panic!("{at}: a top-level `let` chain starts at `node.as_x_node()`"));
    let mut types = vec![ty];
    loop {
        rest = rest.trim_start();
        if rest.is_empty() {
            return types;
        }
        let call = rest.strip_prefix('.').unwrap_or_else(|| panic!("{at}: `{rest}`")).trim_start();
        let name = ident_at(call, 0);
        let args = call[name.len()..].trim_start();
        assert!(args.starts_with('('), "{at}: `.{name}` is not a call");
        let close = close_of(args.as_bytes(), 0);
        let inner = args[1..close].trim();
        rest = &args[close + 1..];
        match name {
            "map" => {}
            "or_else" => {
                let link = inner.strip_prefix("||").unwrap_or_else(|| panic!("{at}: `{inner}`"));
                let link = link.trim();
                let link = match link.strip_prefix('{') {
                    Some(braced) if close_of(link.as_bytes(), 0) == link.len() - 1 => {
                        &braced[..braced.len() - 1]
                    }
                    _ => link,
                };
                types.extend(chain_types(link, at));
            }
            _ => panic!("{at}: a `let` chain links only `.map` and `.or_else`, not `.{name}`"),
        }
    }
}

/// `node.as_x_node()` at the start of `s`: the Prism type (`XNode`) and the
/// rest of `s`.
fn as_node_call(s: &str) -> Option<(String, &str)> {
    let s = s.trim_start();
    if ident_at(s, 0) != "node" {
        return None;
    }
    let s = s["node".len()..].trim_start().strip_prefix('.')?.trim_start();
    let method = ident_at(s, 0);
    let snake = method.strip_prefix("as_")?.strip_suffix("_node")?;
    let rest = s[method.len()..].trim_start().strip_prefix('(')?.trim_start().strip_prefix(')')?;
    let camel: String = snake
        .split('_')
        .map(|w| w[..1].to_ascii_uppercase() + &w[1..])
        .collect();
    Some((camel + "Node", rest))
}

/// The byte span of `fn name`'s body in `text`: its `{` and its `}`.
fn fn_body(text: &str, name: &str, file: &str) -> (usize, usize) {
    let sig = format!("fn {name}(");
    let mut found = text.match_indices(&sig);
    let (start, _) = found.next().unwrap_or_else(|| panic!("{file}: no `{sig}`"));
    assert!(found.next().is_none(), "{file}: `{sig}` twice");
    let open = start + text[start..].find('{').unwrap_or_else(|| panic!("{file}: `{sig}` body"));
    (open, close_of(text.as_bytes(), open))
}

/// `s` split at each `sep` outside brackets.
fn split_top<'a>(s: &'a str, sep: &str) -> Vec<&'a str> {
    let mut parts = Vec::new();
    let mut from = 0;
    while let Some(at) = find_top(s.as_bytes(), from, sep.as_bytes()) {
        parts.push(&s[from..at]);
        from = at + sep.len();
    }
    parts.push(&s[from..]);
    parts
}

/// The first `needle` at or after `from` outside brackets. A lone `=`
/// skips `==`, `!=`, `<=`, `>=` and `=>`.
fn find_top(s: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    let mut depth = 0usize;
    for (i, &c) in s.iter().enumerate().skip(from) {
        if depth == 0 && s[i..].starts_with(needle) {
            let lone_eq = needle == b"=";
            let next = s.get(i + 1).copied().unwrap_or(b' ');
            let prev = if i > 0 { s[i - 1] } else { b' ' };
            if !lone_eq || (!b"=>".contains(&next) && !b"=!<>".contains(&prev)) {
                return Some(i);
            }
        }
        match c {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' if depth == 0 => return None,
            b')' | b']' | b'}' => depth -= 1,
            _ => {}
        }
    }
    None
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

fn is_ident(s: &str) -> bool {
    !s.is_empty() && ident_at(s, 0).len() == s.len()
}

/// `src` with every comment, and the contents of every string and char
/// literal, overwritten with spaces. Newlines survive, so offsets and line
/// numbers still match `src`.
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
