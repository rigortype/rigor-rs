//! ADR-0034: `rbs collection` discovery — a pure filesystem + YAML walk that
//! finds the per-gem RBS directories a project pulled in with `rbs collection
//! install`, so they can be ingested through the ADR-0033 project-`sig/` path.
//!
//! This is a faithful port of the reference's
//! `Rigor::Environment::RbsCollectionDiscovery`, which is itself documented as
//! "a pure file-system + YAML walk — no Bundler API call, no network access".
//! So the port keeps the single-binary, Ruby-free contract (ADR-0001/0007): it
//! reads `rbs_collection.lock.yaml` + walks `.gem_rbs_collection/<name>/
//! <version>/`, and the discovered `.rbs` are parsed by the same native
//! `ruby-rbs` parser as everything else.
//!
//! Every failure mode (missing / malformed lockfile, absent collection root, a
//! listed gem dir not on disk) degrades to an empty list — the run proceeds with
//! no gem RBS, never crashes (ADR-0016).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// The default collection directory, relative to the lockfile, when the lockfile
/// omits an explicit `path:` (mirrors the RBS ecosystem default).
const DEFAULT_COLLECTION_PATH: &str = ".gem_rbs_collection";

/// Source types skipped when collecting gem dirs. A `stdlib`-typed entry names a
/// library rigor-rs already loads via its bundled stdlib closure; ingesting a
/// second copy (possibly at a different `rbs` version) would be a divergence
/// source. Mirrors the reference's `SKIPPED_SOURCE_TYPES`.
fn is_skipped_source_type(source_type: &str) -> bool {
    source_type == "stdlib"
}

/// Discover the per-gem RBS directories declared in a project's
/// `rbs_collection.lock.yaml`. Returns every existing
/// `<collection_root>/<name>/<version>/` directory whose lockfile entry has a
/// non-skipped source type. Empty when no lockfile is resolvable, the YAML is
/// unreadable, or the collection root is absent.
///
/// `lockfile` is an explicit path (from config); when `None` and `auto_detect`
/// is true, `<project_root>/rbs_collection.lock.yaml` is used if it exists.
#[cfg(test)]
pub fn discover(lockfile: Option<&Path>, project_root: &Path, auto_detect: bool) -> Vec<PathBuf> {
    discover_skipping(lockfile, project_root, auto_detect, &BTreeSet::new())
}

/// [`discover`] minus the gems named in `skip` — upstream v0.4.7 (#2441):
/// the project's own `path:` gems ([`project_path_gems`]), whose collection
/// copy would double-declare the project's `sig/`.
pub fn discover_skipping(
    lockfile: Option<&Path>,
    project_root: &Path,
    auto_detect: bool,
    skip: &BTreeSet<String>,
) -> Vec<PathBuf> {
    let Some(lock) = resolve_lockfile_path(lockfile, project_root, auto_detect) else {
        return Vec::new();
    };
    let Ok(text) = std::fs::read_to_string(&lock) else {
        return Vec::new();
    };
    let Ok(data) = serde_yaml::from_str::<serde_yaml::Value>(&text) else {
        return Vec::new();
    };
    if !data.is_mapping() {
        return Vec::new();
    }
    let collection_root = resolve_collection_root(&lock, &data);
    if !collection_root.is_dir() {
        return Vec::new();
    }
    gem_paths_from(&collection_root, &data, skip)
}

/// The resolved lockfile path, or `None`. An explicit `lockfile` is resolved
/// relative to `project_root` (absolute paths pass through) and must exist;
/// otherwise, when `auto_detect`, `<project_root>/rbs_collection.lock.yaml` is
/// used if present.
fn resolve_lockfile_path(
    lockfile: Option<&Path>,
    project_root: &Path,
    auto_detect: bool,
) -> Option<PathBuf> {
    if let Some(lf) = lockfile {
        let path = if lf.is_absolute() {
            lf.to_path_buf()
        } else {
            project_root.join(lf)
        };
        return path.is_file().then_some(path);
    }
    if !auto_detect {
        return None;
    }
    let candidate = project_root.join("rbs_collection.lock.yaml");
    candidate.is_file().then_some(candidate)
}

/// The collection root: the lockfile's `path:` (default `.gem_rbs_collection`),
/// resolved relative to the directory holding the lockfile — the RBS ecosystem's
/// documented `path:` semantics.
fn resolve_collection_root(lockfile: &Path, data: &serde_yaml::Value) -> PathBuf {
    let rel = data
        .get("path")
        .and_then(serde_yaml::Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_COLLECTION_PATH);
    lockfile.parent().unwrap_or(Path::new(".")).join(rel)
}

/// Every `<collection_root>/<name>/<version>/` directory listed under `gems:`
/// whose source type is not skipped, whose `name`/`version` are present, and
/// whose directory exists on disk.
fn gem_paths_from(
    collection_root: &Path,
    data: &serde_yaml::Value,
    skip: &BTreeSet<String>,
) -> Vec<PathBuf> {
    let Some(gems) = data.get("gems").and_then(serde_yaml::Value::as_sequence) else {
        return Vec::new();
    };
    gems.iter()
        .filter_map(|entry| {
            entry.as_mapping()?;
            let source_type = entry
                .get("source")
                .and_then(|s| s.get("type"))
                .and_then(serde_yaml::Value::as_str)
                .unwrap_or("");
            if is_skipped_source_type(source_type) {
                return None;
            }
            let name = scalar_to_string(entry.get("name")?)?;
            if skip.contains(&name) {
                return None;
            }
            let version = scalar_to_string(entry.get("version")?)?;
            let gem_root = collection_root.join(&name).join(&version);
            gem_root.is_dir().then_some(gem_root)
        })
        .collect()
}

/// Upstream v0.4.7 (#2441/#2592, `environment/project_path_gems.rb`): the
/// gems a `Gemfile.lock` `PATH` stanza locks from a directory the run analyses
/// as project source — the gem directory is at or under a `paths:` entry, or
/// contains one, and the file walker keeps at least one `.rb` of the covered
/// part after `exclude:`.
///
/// Only the auto-detected `<root>/Gemfile.lock` is read, and only when
/// `<root>/Gemfile` exists (the reference's Bundler parser raises
/// `GemfileNotFound` without one and reads NO lockfile) and every top-level
/// section is one this line parser understands. Anything else is the empty
/// set: the collection copy stays, which is the pre-#2441 behaviour.
pub fn project_path_gems(
    root: &Path,
    paths: &[String],
    excludes: &crate::config::ExcludeSet,
) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    if !root.join("Gemfile").is_file() {
        return out;
    }
    let lock = root.join("Gemfile.lock");
    let Ok(text) = std::fs::read_to_string(&lock) else {
        return out;
    };
    let Some(gems) = path_stanza_gems(&text) else {
        return out;
    };
    let abs_root = std::path::absolute(root).unwrap_or_else(|_| root.to_path_buf());
    let real = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let entries: Vec<(PathBuf, PathBuf)> = paths
        .iter()
        .map(|p| {
            let logical = fold(&abs_root.join(p));
            let r = real(&logical);
            (logical, r)
        })
        .collect();
    if entries.is_empty() {
        return out;
    }
    for (name, remote) in gems {
        let dir = real(&fold(&abs_root.join(&remote)));
        let reached = entries.iter().any(|(logical, real_entry)| {
            let part = if real_entry.starts_with(&dir) {
                logical.clone()
            } else if let Ok(rest) = dir.strip_prefix(real_entry) {
                logical.join(rest)
            } else {
                return false;
            };
            analysed_ruby(&part, excludes)
        });
        if reached {
            out.insert(name);
        }
    }
    out
}

/// `File.expand_path`'s lexical `.`/`..` fold of an absolute path.
fn fold(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::from("/");
    for c in path.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(n) => out.push(n),
            _ => {}
        }
    }
    out
}

/// Whether the walker would analyse some `.rb` of `part`: the file itself, or
/// a `**/*.rb` under the directory, that `exclude:` keeps.
fn analysed_ruby(part: &Path, excludes: &crate::config::ExcludeSet) -> bool {
    let mut files = Vec::new();
    if part.is_file() {
        files.push(part.to_string_lossy().into_owned());
    } else if part.is_dir() {
        crate::collect_rb_files(part, &mut files);
    }
    files.retain(|f| f.ends_with(".rb"));
    excludes.retain_unexcluded(&mut files);
    !files.is_empty()
}

/// `(name, remote)` for every spec a `PATH` stanza locks, or `None` when the
/// lockfile has a top-level section this parser does not model (Bundler may
/// read it differently). A `GIT` stanza is not a path source.
fn path_stanza_gems(text: &str) -> Option<Vec<(String, String)>> {
    let mut out = Vec::new();
    let mut section = "";
    let mut remote: Option<String> = None;
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            continue;
        }
        if !line.starts_with(' ') {
            if !matches!(
                line,
                "GEM" | "PATH" | "GIT" | "PLATFORMS" | "DEPENDENCIES" | "RUBY VERSION" | "BUNDLED WITH" | "CHECKSUMS"
            ) {
                return None;
            }
            section = if line == "PATH" { "PATH" } else { "" };
            remote = None;
            continue;
        }
        if section != "PATH" {
            continue;
        }
        if let Some(r) = line.strip_prefix("  remote: ") {
            remote = Some(r.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("    ") {
            if rest.starts_with(' ') {
                continue;
            }
            let name = rest.split(' ').next().unwrap_or("");
            match &remote {
                Some(r) if !name.is_empty() => out.push((name.to_string(), r.clone())),
                _ => {}
            }
        }
    }
    Some(out)
}

/// Coerce a YAML scalar (`name`/`version`) to a string. YAML types an unquoted
/// `version: 1.0` as a number, so accept string/int/float — the reference does
/// the same via `to_s`. Non-scalar (or `null`) yields `None`.
fn scalar_to_string(value: &serde_yaml::Value) -> Option<String> {
    match value {
        serde_yaml::Value::String(s) => Some(s.clone()),
        serde_yaml::Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Create a unique temp collection layout under a per-test dir. Returns the
    /// project root. `label` keeps parallel tests from sharing a path.
    fn setup(label: &str, lock_yaml: &str, gem_rel_dirs: &[&str]) -> PathBuf {
        let root = std::env::temp_dir()
            .join(format!("rigor-collection-{}-{label}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("rbs_collection.lock.yaml"), lock_yaml).unwrap();
        for rel in gem_rel_dirs {
            let dir = root.join(rel);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("g.rbs"), "class G\nend\n").unwrap();
        }
        root
    }

    #[test]
    fn discovers_git_and_rubygems_gem_dirs() {
        let yaml = "path: .gem_rbs_collection\n\
                    gems:\n\
                    \x20 - name: mygem\n\
                    \x20   version: \"1.0\"\n\
                    \x20   source:\n\
                    \x20     type: git\n\
                    \x20 - name: other\n\
                    \x20   version: \"2.3\"\n\
                    \x20   source:\n\
                    \x20     type: rubygems\n";
        let root = setup(
            "git-rubygems",
            yaml,
            &[".gem_rbs_collection/mygem/1.0", ".gem_rbs_collection/other/2.3"],
        );
        let mut dirs = discover(None, &root, true);
        dirs.sort();
        assert_eq!(
            dirs,
            vec![
                root.join(".gem_rbs_collection/mygem/1.0"),
                root.join(".gem_rbs_collection/other/2.3"),
            ]
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn skips_stdlib_source_type() {
        let yaml = "gems:\n\
                    \x20 - name: logger\n\
                    \x20   version: \"1.0\"\n\
                    \x20   source:\n\
                    \x20     type: stdlib\n";
        let root = setup("stdlib-skip", yaml, &[".gem_rbs_collection/logger/1.0"]);
        assert!(discover(None, &root, true).is_empty(), "stdlib source type is skipped");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn skips_gem_dir_absent_on_disk() {
        // Listed but the directory was never created ⇒ not returned.
        let yaml = "gems:\n\
                    \x20 - name: ghost\n\
                    \x20   version: \"9.9\"\n\
                    \x20   source:\n\
                    \x20     type: git\n";
        let root = setup("absent", yaml, &[]);
        // The collection root itself doesn't exist ⇒ empty.
        assert!(discover(None, &root, true).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn auto_detect_off_without_explicit_lockfile_is_empty() {
        let yaml = "gems: []\n";
        let root = setup("no-autodetect", yaml, &[]);
        assert!(discover(None, &root, false).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_lockfile_is_empty() {
        let root = std::env::temp_dir()
            .join(format!("rigor-collection-{}-missing", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        assert!(discover(None, &root, true).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }
}
