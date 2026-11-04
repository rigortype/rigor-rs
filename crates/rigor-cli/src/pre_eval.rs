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
/// `Dir.glob` does (a brace group, a malformed pattern): the caller then
/// cannot know which files the reference reads.
fn expand_entry(entry: &str) -> Option<Vec<PathBuf>> {
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
    let count = (1..segments.len()).take_while(|&n| Path::new(&join(n)).is_dir()).count();
    let (base, pattern) = if count == 0 {
        (None, entry.to_string())
    } else {
        (Some(join(count)), segments[count..].join("/"))
    };
    if !glob_pattern(&pattern) {
        return Some(vec![PathBuf::from(entry)]);
    }
    if pattern.contains(['{', '}']) {
        return None;
    }
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
    // A superset is the safe side: every registry read only declines, so a
    // case-insensitive match or a dotfile the reference's glob skips costs
    // coverage, never a diagnostic.
    let options = glob::MatchOptions {
        case_sensitive: false,
        require_literal_separator: true,
        require_literal_leading_dot: false,
    };
    let mut out: Vec<PathBuf> = glob::glob_with(&full, options).ok()?.filter_map(Result::ok).collect();
    out.sort();
    Some(out)
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
        let Ok(bytes) = std::fs::read(&path) else { continue };
        scans.push(scan_pre_eval(&bytes));
    }
    let patches = PreEvalPatches::build(scans);
    (!patches.is_empty()).then(|| Arc::new(patches))
}
