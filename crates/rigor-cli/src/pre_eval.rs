//! ADR-17 — loading the `.rigor.yml` `pre_eval:` files into the
//! [`rigor_infer::PreEvalPatches`] registry every project-aware entry point
//! hands its [`rigor_infer::SourceIndex`] (`check`, the language server,
//! `mcp`, `coverage`, `type-of`, `annotate`). The reference builds the same
//! registry once per configuration
//! (`ProjectEnvironment.project_patched_methods`) and threads it into every
//! probe command.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rigor_infer::PreEvalPatches;
use rigor_parse::pre_eval::{scan_pre_eval, PreEvalScanError};

/// `Configuration#glob_pattern?`.
fn glob_pattern(path: &str) -> bool {
    path.contains(['*', '?', '['])
}

/// `Configuration#expand_pre_eval_entry` (#1956): the leading run of
/// segments naming existing directories is read literally and only the rest
/// is globbed. `None` when the port cannot expand the pattern the way Ruby's
/// `Dir.glob` does (a brace group, a `\` escape the `glob` crate reads
/// literally, a malformed pattern): the caller then cannot know which files
/// the reference reads.
///
/// The glob match options are a parameter because the two consumers sit on
/// OPPOSITE sides of `check`'s zero-FP bar:
///
/// - [`expand_entry`] (the patch registry) reads the reference's own match
///   set — a file `Dir.glob` does not return must not be read (rigor-rs#491
///   item 4). Its options match the reference's `Dir.glob` (no flags): a
///   leading `.` only matches literally, and case folding follows the
///   platform — probed (ruby 4.0.5): `a*` matches `A.rb` on the default
///   case-insensitive APFS volume and matches nothing on a case-sensitive
///   filesystem, while `File.fnmatch` stays case-sensitive on both. The
///   crate cannot tell a case-sensitive volume apart on macOS, so it folds
///   there unconditionally: on such a volume the registry over-reads — a
///   coverage loss (every registry read only declines), never a diagnostic.
/// - [`expand_entry_for_rows`] (the run-level `pre-eval.file-not-found`
///   stream, #345) must read a SUBSET — an extra match that is not a file
///   would emit a row the reference does not. It therefore matches
///   case-sensitively on every platform: exact on a case-sensitive
///   filesystem (CI's Linux runners), and on a case-insensitive volume it
///   can only MISS the row for a case-mismatched directory name — a
///   coverage gap, never an FP.
fn expand_entry_with(entry: &str, options: glob::MatchOptions) -> Option<Vec<PathBuf>> {
    if !glob_pattern(entry) {
        return Some(vec![PathBuf::from(entry)]);
    }
    let segments: Vec<&str> = entry.split('/').collect();
    let join = |n: usize| {
        let p = segments[..n].join("/");
        if p.is_empty() {
            "/".to_string()
        } else {
            p
        }
    };
    let count = (1..segments.len())
        .take_while(|&n| Path::new(&join(n)).is_dir())
        .count();
    let (base, pattern) = if count == 0 {
        (None, entry.to_string())
    } else {
        (Some(join(count)), segments[count..].join("/"))
    };
    if !glob_pattern(&pattern) {
        return Some(vec![PathBuf::from(entry)]);
    }
    let pattern = dir_glob_syntax(&pattern)?;
    let full = match &base {
        Some(b) => {
            let b = glob::Pattern::escape(b);
            if b.ends_with('/') {
                format!("{b}{pattern}")
            } else {
                format!("{b}/{pattern}")
            }
        }
        None => pattern,
    };
    let mut out: Vec<PathBuf> = glob::glob_with(&full, options)
        .ok()?
        .filter_map(Result::ok)
        .collect();
    out.sort();
    Some(out)
}

/// `Dir.glob`'s (no-flags) pattern syntax, translated to the `glob` crate's:
/// `[^` at a class head negates exactly like `[!` (the only form the crate
/// reads — `Dir.glob("[^a]*")` and `Dir.glob("[!a]*")` match the same set),
/// so a `[` immediately followed by `^` is rewritten. A pattern the crate
/// cannot reproduce at all — a `\` escape it would read literally, a `{a,b}`
/// brace group it does not expand — is `None`, the unexpandable verdict the
/// callers already carry.
fn dir_glob_syntax(pattern: &str) -> Option<String> {
    if pattern.contains(['{', '}']) || pattern.contains('\\') {
        return None;
    }
    let mut out = String::with_capacity(pattern.len());
    let mut in_class = false;
    let mut it = pattern.chars().peekable();
    while let Some(c) = it.next() {
        if !in_class && c == '[' {
            in_class = true;
            out.push('[');
            if it.peek() == Some(&'^') {
                it.next();
                out.push('!');
            }
            continue;
        }
        if in_class && c == ']' {
            in_class = false;
        }
        out.push(c);
    }
    Some(out)
}

/// `Dir.glob` (no flags) folds case where the filesystem does — macOS and
/// Windows volumes by default, never the case-sensitive filesystems the
/// Linux runners and a case-sensitive volume mount give it. Compiled per
/// platform: the `glob` crate's userspace matcher has no volume query.
const GLOB_CASE_SENSITIVE: bool = cfg!(all(unix, not(target_os = "macos")));

/// The registry-side expansion (see [`expand_entry_with`] for the stance).
pub(crate) fn expand_entry(entry: &str) -> Option<Vec<PathBuf>> {
    expand_entry_with(
        entry,
        glob::MatchOptions {
            case_sensitive: GLOB_CASE_SENSITIVE,
            require_literal_separator: true,
            require_literal_leading_dot: true,
        },
    )
}

/// The row-side expansion of one `pre_eval:` entry (#345) — the members the
/// reference's expanded stream holds for it
/// (`Configuration#expand_pre_eval_entries`, over which the aggregator's
/// `File.file?` runs): a literal — or rooted-literal — entry is itself, a
/// glob entry its sorted matches, and `None` a brace group or malformed
/// pattern whose match set the port cannot reproduce (such an entry earns no
/// row — declining, never an FP).
pub(crate) fn expand_entry_for_rows(entry: &str) -> Option<Vec<String>> {
    expand_entry_with(
        entry,
        glob::MatchOptions {
            case_sensitive: true,
            require_literal_separator: true,
            require_literal_leading_dot: true,
        },
    )
    .map(|paths| {
        paths
            .into_iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect()
    })
}

/// The concrete paths the `pre_eval:` entries expand to right now — each
/// literal entry itself (an unexpandable entry yields nothing) and each
/// glob its [`expand_entry`] matches. The LSP's watched-file surface reads
/// this to classify a `pre_eval:` file's edit as structural (upstream
/// #1613): the same expansion [`load`] scans, so the set is exactly the
/// files whose contents the registry was built from.
pub(crate) fn expanded_files(entries: &[String]) -> Vec<PathBuf> {
    entries
        .iter()
        .filter_map(|e| expand_entry(e))
        .flatten()
        .collect()
}

/// Scan every configured `pre_eval:` file that is a file on disk, in
/// configuration order (`expand_pre_eval_entries` + the runner's
/// `File.file?` filter + `ProjectPatchedScanner.scan`). `None` when the
/// project lists no `pre_eval:` entry or no file patches anything.
pub(crate) fn load(entries: &[String]) -> Option<Arc<PreEvalPatches>> {
    if entries.is_empty() {
        return None;
    }
    let mut files: Vec<PathBuf> = Vec::new();
    let mut scans = Vec::new();
    for entry in entries {
        match expand_entry(entry) {
            Some(paths) => {
                for p in paths {
                    if !files.contains(&p) {
                        files.push(p);
                    }
                }
            }
            None => scans.push(Err(PreEvalScanError::TooDeep)),
        }
    }
    for path in files {
        if !path.is_file() {
            continue;
        }
        // An unreadable file is skipped, as the reference's `scan_file`
        // rescue skips it.
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        scans.push(scan_pre_eval(&bytes));
    }
    let patches = PreEvalPatches::build(scans);
    (!patches.is_empty()).then(|| Arc::new(patches))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_entry_passes_through() {
        assert_eq!(
            expand_entry("/no/such/dir/patch.rb"),
            Some(vec![PathBuf::from("/no/such/dir/patch.rb")])
        );
    }

    #[test]
    fn glob_entry_expands_sorted_under_a_literal_directory() {
        let dir =
            std::env::temp_dir().join(format!("rigor-pre-eval-glob-[x]-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("ext")).unwrap();
        std::fs::write(dir.join("ext/b.rb"), "class String; def gb = 1; end\n").unwrap();
        std::fs::write(dir.join("ext/a.rb"), "class String; def ga = 1; end\n").unwrap();
        let entry = format!("{}/ext/*.rb", dir.display());
        let got = expand_entry(&entry).unwrap();
        assert_eq!(got, vec![dir.join("ext/a.rb"), dir.join("ext/b.rb")]);
        let patches = load(&[entry]).expect("patches");
        assert!(patches.defines_name("ga") && patches.defines_name("gb"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_brace_pattern_the_port_cannot_expand_reads_as_unknown() {
        assert_eq!(expand_entry("/tmp/{a,b}/*.rb"), None);
        let patches =
            load(&["/tmp/rigor-no-such-{a,b}/*.rb".to_string()]).expect("opaque registry");
        assert!(patches.defines_name("anything"));
    }

    /// #345 — the row-side expansion reads a SUBSET of the reference's set:
    /// a dot-leading name never matches a wildcard (`Dir.glob("*.rb")`
    /// skips `.hidden.rb` — probed), and case matching stays case-sensitive
    /// (the reference's `Dir.glob` folds case only on case-insensitive
    /// VOLUMES — probed on both volume kinds: `Dir.glob("a*")` matches
    /// `A.rb` on the default volume, nothing on a case-sensitive one), so
    /// neither kind of name can earn a row the reference does not. The
    /// registry side reads the reference's set on both axes: the dot-leading
    /// name is never read, and the case-mismatched name only where the
    /// platform's `Dir.glob` folds.
    #[test]
    fn rows_expansion_never_reads_what_the_reference_skips() {
        let dir = std::env::temp_dir().join(format!("rigor-pre-eval-rows-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".h.d")).unwrap();
        std::fs::create_dir_all(dir.join("Setup.d")).unwrap();
        let star = format!("{}/*", dir.display());
        let setup = format!("{}/setup*", dir.display());

        for side in [expand_entry(&star), expand_entry_for_rows(&star).map(|v| {
            v.into_iter().map(PathBuf::from).collect()
        })] {
            let side = side.unwrap();
            assert!(
                !side.iter().any(|p| p.ends_with(".h.d")),
                "a dot-leading name is never read: {side:?}"
            );
        }
        let registry_case = expand_entry(&setup).unwrap();
        assert_eq!(
            registry_case.iter().any(|p| p.ends_with("Setup.d")),
            !GLOB_CASE_SENSITIVE,
            "the registry folds the case-mismatched name exactly where the platform's Dir.glob does: {registry_case:?}"
        );
        let rows_case = expand_entry_for_rows(&setup).unwrap();
        assert!(
            !rows_case.iter().any(|p| p.ends_with("Setup.d")),
            "a case-mismatched name never earns a row: {rows_case:?}"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// `[^` at a class head negates like `[!` (`Dir.glob("[^a]*")` skips
    /// `a.rb`) — the `glob` crate reads only `!`, so the syntax is
    /// translated; a `^` that is not the class's first char stays literal.
    #[test]
    fn caret_negation_translates() {
        assert_eq!(dir_glob_syntax("[^a]*.rb").as_deref(), Some("[!a]*.rb"));
        assert_eq!(dir_glob_syntax("[!a]*.rb").as_deref(), Some("[!a]*.rb"));
        assert_eq!(dir_glob_syntax("[a^]*.rb").as_deref(), Some("[a^]*.rb"));
        assert_eq!(dir_glob_syntax("x[^ab]").as_deref(), Some("x[!ab]"));
        // Unexpandable shapes stay unexpandable: braces, a `\` escape.
        assert_eq!(dir_glob_syntax("{a,b}/*.rb"), None);
        assert_eq!(dir_glob_syntax("a\\*.rb"), None);
    }

    #[test]
    fn caret_negated_class_expands() {
        let dir = std::env::temp_dir().join(format!("rigor-pre-eval-caret-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.rb"), "").unwrap();
        std::fs::write(dir.join("b.rb"), "").unwrap();
        let pat = format!("{}/[^a]*.rb", dir.display());
        let got = expand_entry(&pat).unwrap();
        assert_eq!(got, vec![dir.join("b.rb")], "[^a] excludes a.rb");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
