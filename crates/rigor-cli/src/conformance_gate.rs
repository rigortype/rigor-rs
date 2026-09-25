//! Issue #129 / ADR-0044 § "Environment-parity gate": whether the run the
//! reference makes is PROVABLY the run the port models, for the
//! `rigor:v1:conforms-to` scan only.
//!
//! Three review rounds found the same lesson: every false positive the port's
//! BUILD model did not cause came from the reference running in a different
//! environment or reading a different configuration (a rejected config, a
//! YAML-1.1 boolean, a `~` path, a glob metacharacter, an rbs collection it
//! skips, a lockfile source the port does not parse, a run with no Ruby file
//! and therefore no environment at all). That set is open-ended, so the scan
//! stands down unless each clause here proves parity; every clause is
//! oracle-justified in the ADR. The index-side clauses (a project `.rbs` the
//! port cannot parse, a NUL byte, `use` / `resolve-type-names`) live in
//! `rigor_index`'s conformance builder.
//!
//! None of these divergences is fixed here for any OTHER rule: they are
//! pre-existing and listed as follow-ups in the ADR.

use std::path::{Component, Path, PathBuf};

// ---------------------------------------------------------------------------
// A strict YAML subset with Psych (YAML 1.1) typing
// ---------------------------------------------------------------------------

/// One scalar as written: its text, and whether it was quoted.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Scalar {
    text: String,
    quoted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Node {
    Null,
    Scalar(Scalar),
    Seq(Vec<Scalar>),
    Map(Vec<(Scalar, Scalar)>),
}

/// Parse `text` as the narrow YAML subset every accepted config must fit:
/// top-level `key: scalar`, `key: []` / `key: {}`, or `key:` followed by ONE
/// level of block sequence (`- scalar`, indented or not) or block mapping
/// (`sub: scalar`). Comments, blank lines, CRLF and a leading `---` are
/// allowed. Anything else — anchors, aliases, tags, flow collections with
/// content, block scalars, multi-line or escaped scalars, tabs, nesting,
/// duplicate keys, a second document — is `None`: the gate cannot prove
/// what Psych makes of it.
fn parse_subset(text: &str) -> Option<Vec<(String, Node)>> {
    let mut lines: Vec<(usize, &str)> = Vec::new();
    for (i, raw) in text.split('\n').enumerate() {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        if line.contains('\t') || line.contains('\r') {
            return None;
        }
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if i == 0 && line.trim_end() == "---" && lines.is_empty() {
            continue;
        }
        if line.starts_with("---") || line.starts_with("...") {
            return None;
        }
        lines.push((line.len() - trimmed.len(), line.trim_end()));
    }
    let mut out: Vec<(String, Node)> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let (indent, line) = lines[i];
        if indent != 0 || line.starts_with('-') {
            return None;
        }
        let (key, rest) = line.split_once(':')?;
        if !key.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            return None;
        }
        if !rest.is_empty() && !rest.starts_with(' ') {
            return None;
        }
        if out.iter().any(|(k, _)| k == key) {
            return None;
        }
        i += 1;
        let rest = strip_comment(rest.trim());
        let node = if rest.is_empty() {
            let mut children: Vec<(usize, &str)> = Vec::new();
            while i < lines.len() && (lines[i].0 > 0 || lines[i].1.starts_with('-')) {
                children.push(lines[i]);
                i += 1;
            }
            block_node(&children)?
        } else if rest == "[]" {
            Node::Seq(Vec::new())
        } else if rest == "{}" {
            Node::Map(Vec::new())
        } else {
            Node::Scalar(scalar(rest)?)
        };
        out.push((key.to_string(), node));
    }
    Some(out)
}

fn block_node(children: &[(usize, &str)]) -> Option<Node> {
    let Some(&(indent, first)) = children.first() else {
        return Some(Node::Null);
    };
    if children.iter().any(|&(ind, _)| ind != indent) {
        return None;
    }
    if first.trim_start().starts_with('-') {
        let mut items = Vec::new();
        for &(_, line) in children {
            let item = line.trim_start().strip_prefix("- ")?;
            items.push(scalar(strip_comment(item.trim()))?);
        }
        return Some(Node::Seq(items));
    }
    if indent == 0 {
        return None;
    }
    let mut entries: Vec<(Scalar, Scalar)> = Vec::new();
    for &(_, line) in children {
        let (k, v) = split_map_entry(line.trim_start())?;
        let v = strip_comment(v.trim());
        if v.is_empty() || entries.iter().any(|(ek, _)| ek.text == k.text) {
            return None;
        }
        entries.push((k, scalar(v)?));
    }
    Some(Node::Map(entries))
}

/// `key: value` inside a block mapping; the key plain or quoted.
fn split_map_entry(line: &str) -> Option<(Scalar, &str)> {
    if let Some(q) = line.chars().next().filter(|c| *c == '"' || *c == '\'') {
        let end = line[1..].find(q)? + 1;
        let key = &line[1..end];
        let rest = line[end + 1..].strip_prefix(':')?;
        if key.contains('\\') || (!rest.is_empty() && !rest.starts_with(' ')) {
            return None;
        }
        return Some((Scalar { text: key.to_string(), quoted: true }, rest));
    }
    let (k, rest) = line.split_once(": ").or_else(|| line.strip_suffix(':').map(|k| (k, "")))?;
    Some((scalar(k)?, rest))
}

/// Drop a trailing ` # comment` (YAML needs the space before `#`) outside
/// quotes.
fn strip_comment(s: &str) -> &str {
    let mut quote: Option<char> = None;
    let mut prev_space = true;
    for (i, c) in s.char_indices() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if (c == '"' || c == '\'') && i == 0 => quote = Some(c),
            None if c == '#' && prev_space => return s[..i].trim_end(),
            None => {}
        }
        prev_space = c == ' ';
    }
    s
}

/// One scalar in the subset: a quoted string without escapes or embedded
/// quotes, or a plain scalar that does not open with a YAML indicator.
fn scalar(s: &str) -> Option<Scalar> {
    let s = s.trim();
    if let Some(q) = s.chars().next().filter(|c| *c == '"' || *c == '\'') {
        let inner = s.strip_prefix(q)?.strip_suffix(q)?;
        if inner.contains(q) || inner.contains('\\') {
            return None;
        }
        return Some(Scalar { text: inner.to_string(), quoted: true });
    }
    let first = s.chars().next()?;
    if "-?:,[]{}#&*!|>'\"%@`".contains(first) || s.contains(": ") || s.ends_with(':') {
        return None;
    }
    Some(Scalar { text: s.to_string(), quoted: false })
}

/// Psych reads this scalar as a `String` equal to its text, and so does
/// `serde_yaml`: a quoted scalar, or a plain one built only from name and
/// path characters that YAML 1.1 does not type (no boolean or null word, no
/// number, no `.inf` / `.nan`, no leading digit, sign or dot-digit).
fn is_string(s: &Scalar) -> bool {
    if s.quoted {
        return true;
    }
    let t = s.text.as_str();
    let mut cs = t.chars();
    let ok_start = match cs.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' || c == '/' => true,
        // `.` alone, `./x`, `.sig` — but not `.5` (a float).
        Some('.') => cs.next().is_none_or(|c| c.is_ascii_alphabetic() || c == '_' || c == '/'),
        _ => false,
    };
    let lower = t.to_ascii_lowercase();
    ok_start
        && t.chars().all(|c| c.is_ascii_alphanumeric() || "_./@+-".contains(c))
        && !matches!(
            lower.as_str(),
            "yes" | "no" | "true" | "false" | "on" | "off" | "null" | ".inf" | ".nan"
        )
}

fn strings(node: &Node) -> Option<Vec<&str>> {
    match node {
        Node::Seq(items) => items.iter().map(|s| is_string(s).then_some(s.text.as_str())).collect(),
        _ => None,
    }
}

/// The top-level keys the reference's `Configuration` owns
/// (`KNOWN_KEYS`: its `DEFAULTS`, `includes`, the reserved `rigor_rs`).
const REFERENCE_KNOWN_KEYS: &[&str] = &[
    "target_ruby", "paths", "exclude", "plugins", "disable", "libraries", "signature_paths",
    "pre_eval", "baseline", "fold_platform_specific_paths", "parameter_inference", "effects",
    "cache", "plugins_isolation", "plugins_io", "severity_profile", "severity_overrides",
    "bleeding_edge", "dependencies", "parallel", "bundler", "rbs_collection", "includes",
    "rigor_rs",
];

/// Whether the config text is one the reference provably loads WITHOUT
/// error and reads the way the port does (read in
/// `lib/rigor/configuration.rb` at the pin). Each accepted key's value shape
/// is one whose coercion was read and probed; every other known key stands
/// the scan down (its validation or its effect on the environment is not
/// modelled), and so does any value outside the subset.
pub(crate) fn config_text_ok(text: &str) -> bool {
    let Some(doc) = parse_subset(text) else {
        return false;
    };
    doc.iter().all(|(key, node)| key_ok(key, node))
}

fn key_ok(key: &str, node: &Node) -> bool {
    match key {
        // `Array(x).map(&:to_s)`; paths are then `File.expand_path`'d.
        "signature_paths" | "paths" => strings(node).is_some(),
        // A bare `key:` is `nil` upstream, `Array(nil) == []`; the port reads
        // an empty list too.
        "exclude" | "disable" => *node == Node::Null || strings(node).is_some(),
        // `coerce_plugin_entry`: a String is a GEM name the loader requires;
        // the port normalises a bare id too, the reference fails to load it.
        "plugins" => *node == Node::Null || strings(node).is_some_and(|ids| {
            ids.iter().all(|id| {
                id.starts_with("rigor-") && rigor_index::plugins::bundled_plugin(id).is_some()
            })
        }),
        // `coerce_target_ruby`: `to_s` must match the version pattern; a
        // plain `3.3e0` is a String to Psych but a float to serde_yaml.
        "target_ruby" => match node {
            Node::Scalar(s) if s.quoted => true,
            Node::Scalar(s) => {
                matches!(s.text.as_str(), "3.3" | "3.4" | "4.0" | "latest")
                    || is_patch_version(&s.text)
            }
            _ => false,
        },
        // `coerce_severity_profile`: `to_sym` in the profile set.
        "severity_profile" => matches!(node, Node::Scalar(s)
            if is_string(s) && matches!(s.text.as_str(), "lenient" | "balanced" | "strict")),
        // `coerce_severity_overrides`: a Hash whose values are Strings in
        // the severity set (a bare `off` is `false` there and raises).
        "severity_overrides" => match node {
            Node::Map(entries) => entries.iter().all(|(k, v)| {
                is_string(k)
                    && is_string(v)
                    && matches!(v.text.as_str(), "error" | "warning" | "info" | "off")
            }),
            _ => false,
        },
        // `coerce_baseline_path`: nil / false, else `to_s`.
        "baseline" => match node {
            Node::Scalar(s) => is_string(s) || (!s.quoted && s.text == "false"),
            _ => false,
        },
        // `coerce_bleeding_edge`: `true` / `false` / a list of ids.
        "bleeding_edge" => match node {
            Node::Scalar(s) => !s.quoted && matches!(s.text.as_str(), "true" | "false"),
            n => strings(n).is_some(),
        },
        // The reserved namespace: never read by the reference.
        "rigor_rs" => match node {
            Node::Map(entries) => entries.iter().all(|(k, v)| is_string(k) && is_string(v)),
            _ => false,
        },
        k if REFERENCE_KNOWN_KEYS.contains(&k) => false,
        // An unknown key is inert there (recorded in `unknown_keys`, only
        // warned about), provided Psych can load its value at all.
        _ => inert_value_ok(node),
    }
}

fn is_patch_version(t: &str) -> bool {
    let parts: Vec<&str> = t.split('.').collect();
    parts.len() == 3 && parts.iter().all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

/// A value `YAML.safe_load` accepts for sure: strings, booleans, plain
/// integers (no date, time, symbol or other disallowed class).
fn inert_value_ok(node: &Node) -> bool {
    let ok = |s: &Scalar| {
        is_string(s)
            || (!s.quoted
                && (matches!(s.text.to_ascii_lowercase().as_str(), "true" | "false" | "yes" | "no" | "on" | "off")
                    || (!s.text.is_empty() && s.text.bytes().all(|b| b.is_ascii_digit()))))
    };
    match node {
        Node::Null => true,
        Node::Scalar(s) => ok(s),
        Node::Seq(items) => items.iter().all(ok),
        Node::Map(entries) => entries.iter().all(|(k, v)| is_string(k) && ok(v)),
    }
}

// ---------------------------------------------------------------------------
// Paths
// ---------------------------------------------------------------------------

/// Ruby's `File.expand_path` for a path without `~`: absolute, with `.` and
/// `..` folded lexically.
fn expand_path(path: &Path) -> PathBuf {
    let abs = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let mut out = PathBuf::new();
    for c in abs.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// A glob metacharacter `Dir.glob` would interpret in a path it is handed.
fn has_glob_meta(s: &str) -> bool {
    s.contains(['*', '?', '[', ']', '{', '}', '\\'])
}

/// Whether the reference reads the same signature directory the port reads
/// for `entry` (as written in the config whose directory is `base`): no `~`
/// (`File.expand_path` expands it), no glob metacharacter anywhere in the
/// absolute path `Dir.glob` receives, and a `..` only where the lexical fold
/// the reference makes names the directory the OS reaches.
pub(crate) fn signature_entry_ok(entry: &str, base: Option<&Path>) -> bool {
    if entry.starts_with('~') {
        return false;
    }
    let port_path = match base {
        Some(b) => b.join(entry),
        None => PathBuf::from(entry),
    };
    let lexical = expand_path(&port_path);
    if has_glob_meta(&lexical.to_string_lossy()) {
        return false;
    }
    if Path::new(entry).components().any(|c| c == Component::ParentDir) {
        return match (std::fs::canonicalize(&port_path), std::fs::canonicalize(&lexical)) {
            (Ok(a), Ok(b)) => a == b,
            (Err(_), Err(_)) => true,
            _ => false,
        };
    }
    true
}

/// Ruby's `File.fnmatch?(pattern, path)` with NO flags, over-approximated:
/// `true` whenever it MIGHT match. `*` spans `/` (no `FNM_PATHNAME`), `?` is
/// one character; a bracket expression or an escape counts as "might match",
/// and the leading-period rule is ignored (both only widen the answer).
fn fnmatch_may(pattern: &str, path: &str) -> bool {
    if pattern.contains(['[', '\\']) {
        return true;
    }
    fn go(p: &[char], s: &[char]) -> bool {
        match p.split_first() {
            None => s.is_empty(),
            Some(('*', rest)) => (0..=s.len()).any(|i| go(rest, &s[i..])),
            Some(('?', rest)) => !s.is_empty() && go(rest, &s[1..]),
            Some((c, rest)) => s.first() == Some(c) && go(rest, &s[1..]),
        }
    }
    let p: Vec<char> = pattern.chars().collect();
    let s: Vec<char> = path.chars().collect();
    go(&p, &s)
}

/// `Configuration::BUILTIN_EXCLUDES`, always appended upstream.
const BUILTIN_EXCLUDES: &[&str] = &["**/vendor/bundle/**", "**/.bundle/**", "**/node_modules/**"];

/// Whether the reference's run has at least one Ruby file (`expand_paths`
/// of its roots is non-empty). Without one it builds no environment and
/// scans nothing (oracle: an empty `lib/`, a missing or `.rbs` argument, an
/// empty `paths:` — no rows). `cli_roots` are the positional arguments
/// (used as given upstream); with none, the roots are the config's `paths:`
/// (`File.expand_path`'d against the config's directory upstream) or the
/// default `lib` (not expanded). Answered as an UNDER-approximation: a root
/// the port cannot place exactly counts as empty.
pub(crate) fn reference_has_ruby_files(
    cli_roots: &[&str],
    config_paths: Option<&[String]>,
    base: Option<&Path>,
    excludes: &[String],
) -> bool {
    let roots: Vec<String> = if !cli_roots.is_empty() {
        cli_roots.iter().map(|s| (*s).to_string()).collect()
    } else if let Some(paths) = config_paths {
        let mut out = Vec::new();
        for p in paths {
            if p.starts_with('~') {
                continue;
            }
            let joined = match base {
                Some(b) => b.join(p),
                None => PathBuf::from(p),
            };
            out.push(expand_path(&joined).to_string_lossy().into_owned());
        }
        out
    } else {
        vec!["lib".to_string()]
    };
    roots.iter().any(|r| root_has_ruby_file(r, excludes))
}

fn root_has_ruby_file(root: &str, excludes: &[String]) -> bool {
    let path = Path::new(root);
    if path.is_dir() {
        // `Dir.glob(File.join(root, "**/*.rb"))`, then `reject_excluded`.
        if has_glob_meta(root) {
            return false;
        }
        let mut files = Vec::new();
        crate::collect_rb_files(path, &mut files);
        files.iter().any(|f| {
            let f = f.as_str();
            !BUILTIN_EXCLUDES.iter().any(|p| fnmatch_may(p, f))
                && !excludes.iter().any(|p| fnmatch_may(p, f))
        })
    } else {
        // An explicit file: `File.file?(path) && path.end_with?(".rb")`,
        // never filtered by `exclude:`.
        path.is_file() && root.ends_with(".rb")
    }
}

// ---------------------------------------------------------------------------
// Project inputs the reference reads and the port does not
// ---------------------------------------------------------------------------

/// Whether a `Gemfile.lock` at the root holds only the sections the port's
/// lockfile reader understands. The reference parses it with Bundler's
/// `LockfileParser`, so a gem locked from a `GIT` or `PATH` source still
/// selects its bundled overlay there (oracle: `activesupport` from GIT loads
/// the overlay upstream) while the port reads only `GEM`.
pub(crate) fn lockfile_ok(root: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(root.join("Gemfile.lock")) else {
        return !root.join("Gemfile.lock").exists();
    };
    text.lines().all(|l| {
        let l = l.trim_end_matches('\r');
        l.is_empty()
            || l.starts_with(' ')
            || matches!(l, "GEM" | "PLATFORMS" | "DEPENDENCIES" | "RUBY VERSION" | "BUNDLED WITH" | "CHECKSUMS")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subset_accepts_ordinary_configs() {
        for ok in [
            "signature_paths:\n  - sig\n",
            "---\n# c\nsignature_paths:\n- sig\n- vendor/rbs # x\nplugins: []\n",
            "signature_paths:\n  - sig\nseverity_profile: strict\nseverity_overrides:\n  rbs_extended: error\n  dynamic.rbs-extended.unresolved: \"off\"\n",
            "signature_paths:\n  - sig\ntarget_ruby: \"3.4\"\nbaseline: .rigor-baseline.yml\nbleeding_edge: true\nfail_on: warning\n",
            "signature_paths:\r\n  - sig\r\npaths:\r\n  - lib\r\n",
            "signature_paths:\n  - sig\nplugins:\n  - rigor-activesupport-core-ext\ntarget_ruby: 4.0\n",
        ] {
            assert!(config_text_ok(ok), "{ok:?}");
        }
    }

    /// PR #150 round 3, family 13: configs the reference rejects or reads
    /// differently (every one oracle-measured) are refused.
    #[test]
    fn subset_refuses_what_the_reference_rejects_or_reads_otherwise() {
        for bad in [
            "signature_paths:\n  - sig\nseverity_overrides:\n  call.undefined-method: off\n",
            "signature_paths:\n  - sig\nseverity_overrides:\n  x: ERROR\n",
            "signature_paths:\n  - sig\nseverity_overrides:\n  x: :error\n",
            "signature_paths:\n  - sig\nseverity_overrides:\n",
            "signature_paths:\n  - sig\nseverity_overrides:\n  - a\n",
            "signature_paths:\n  - sig\nseverity_profile: bogus\n",
            "signature_paths:\n  - sig\nseverity_profile: :strict\n",
            "signature_paths:\n  - sig\nparallel:\n  workers: -3\n",
            "signature_paths:\n  - sig\nplugins_isolation: bogus\n",
            "signature_paths:\n  - sig\ncache: 5\n",
            "signature_paths:\n  - sig\ndependencies: 5\n",
            "x: &a sig\nsignature_paths:\n  - *a\n",
            "signature_paths:\n  - sig\nnote: 2024-01-01\n",
            "signature_paths:\n  - sig\ntarget_ruby: 3.3e0\n",
            "signature_paths:\n  - off\n",
            "signature_paths:\n  - yes\n",
            "signature_paths:\n  - 1_0\n",
            "signature_paths:\n  - nope\nsignature_paths:\n  - sig\n",
            "signature_paths:\n  - sig\nplugins:\n  - activesupport-core-ext\n",
            "signature_paths: sig\n",
            "signature_paths:\n  - sig\nlibraries:\n  - json\n",
            "signature_paths:\n  - sig\nincludes:\n  - other.yml\n",
            "signature_paths:\n  - sig\nrbs_collection:\n  auto_detect: false\n",
            "signature_paths: [sig]\n",
            "signature_paths:\n  - !!str sig\n",
            "signature_paths:\n\t- sig\n",
        ] {
            assert!(!config_text_ok(bad), "{bad:?}");
        }
    }

    #[test]
    fn fnmatch_over_approximates_ruby() {
        assert!(fnmatch_may("**/vendor/bundle/**", "x/vendor/bundle/a.rb"));
        assert!(!fnmatch_may("**/vendor/bundle/**", "vendor/bundle/a.rb"));
        assert!(fnmatch_may("app.rb", "app.rb"));
        assert!(fnmatch_may("lib/*", "lib/a/b.rb"));
        assert!(!fnmatch_may("lib/*.rb", "app/a.rb"));
        assert!(fnmatch_may("[a]pp.rb", "zzz"));
    }

    /// Family 9: no Ruby file ⇒ the reference builds no environment and
    /// reports nothing (oracle: empty `lib/`, a missing or `.rbs` argument).
    #[test]
    fn ruby_file_presence_matches_expand_paths() {
        let dir = std::env::temp_dir().join(format!("rigor_gate_files_{}", std::process::id()));
        std::fs::create_dir_all(dir.join("empty")).unwrap();
        std::fs::create_dir_all(dir.join("lib/sub")).unwrap();
        std::fs::write(dir.join("lib/sub/a.rb"), "x = 1\n").unwrap();
        std::fs::write(dir.join("a.rbs"), "").unwrap();
        let s = |p: &str| dir.join(p).to_string_lossy().into_owned();
        let has = |roots: &[&str], ex: &[&str]| {
            let ex: Vec<String> = ex.iter().map(|e| (*e).to_string()).collect();
            reference_has_ruby_files(roots, None, None, &ex)
        };
        assert!(has(&[&s("lib")], &[]));
        assert!(has(&[&s("lib/sub/a.rb")], &["*"])); // explicit files are never excluded
        assert!(!has(&[&s("lib")], &["*/sub/*"]));
        assert!(!has(&[&s("empty")], &[]));
        assert!(!has(&[&s("a.rbs")], &[]));
        assert!(!has(&[&s("nope.rb")], &[]));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Family 8: a lockfile section the port does not parse (Bundler reads
    /// a `GIT` / `PATH` gem as locked; the port reads `GEM` only).
    #[test]
    fn lockfile_sections() {
        let dir = std::env::temp_dir().join(format!("rigor_gate_lock_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(lockfile_ok(&dir));
        std::fs::write(dir.join("Gemfile.lock"), "GEM\n  remote: x\n  specs:\n    a (1)\n\nPLATFORMS\n  ruby\n\nDEPENDENCIES\n  a\n").unwrap();
        assert!(lockfile_ok(&dir));
        std::fs::write(dir.join("Gemfile.lock"), "GIT\n  remote: x\n  specs:\n    activesupport (8)\n\nGEM\n  specs:\n").unwrap();
        assert!(!lockfile_ok(&dir));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn signature_entries_need_parity() {
        assert!(!signature_entry_ok("~/sig", None));
        assert!(!signature_entry_ok("sig[1]", None));
        assert!(!signature_entry_ok("s{x}", None));
        assert!(signature_entry_ok("sig", None));
    }
}
