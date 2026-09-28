//! Issue #163 — byte-identity of `check --format text|github|sarif` with the
//! reference. The expected strings below are the reference's stdout, captured
//! at the pinned submodule (v0.3.9) with
//! `ruby -I reference/rigor/lib -I reference/rigor/plugins/rigor-rbs-inline/lib
//! reference/rigor/exe/rigor check --no-cache --no-stats --format F a.rb b.rb 'c,d:e.rb'`
//! over the fixture [`project`] writes, in a fresh cwd.
//!
//! The one deliberate difference is the SARIF driver `version`: the reference
//! reports `Rigor::VERSION`, the port its own release version (the value
//! `rigor --version` prints), substituted for `{VERSION}` here.
//!
//! The fixture covers several rows across files, a warning mixed with errors
//! (so the summary counts errors only, and files carrying an error only), and
//! a path and message carrying `%`, `,` and `:` (github escaping).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "rigor-{tag}-{}-{}-{n}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&path).expect("create temp dir");
        TempDir(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn project() -> TempDir {
    let dir = TempDir::new("output-formats");
    let p = dir.path();
    fs::write(p.join(".rigor.yml"), "severity_overrides:\n  call.wrong-arity: warning\n").unwrap();
    fs::write(p.join("a.rb"), "\"abc\".lenght\n1.upcase\n").unwrap();
    fs::write(p.join("b.rb"), "\"x\".upcase(1, 2, 3)\n").unwrap();
    fs::write(p.join("c,d:e.rb"), "\"%,:\".fooo\n").unwrap();
    fs::write(p.join("clean.rb"), "x = 1\nputs x\n").unwrap();
    dir
}

/// `(stdout, stderr, exit)` of `rigor check ARGS` in `cwd`. Hermetic: the
/// Ruby-free sound subset (`RIGOR_NO_RUBY`) fires every fixture row, and CI
/// auto-detection is off so a `GITHUB_ACTIONS` runner does not append its
/// annotations to the text output.
fn check(cwd: &Path, args: &[&str]) -> (String, String, i32) {
    let out = Command::new(env!("CARGO_BIN_EXE_rigor"))
        .current_dir(cwd)
        .env("RIGOR_NO_RUBY", "1")
        .env("RIGOR_CI_DETECT", "0")
        .env_remove("RIGOR_RUBY")
        .env_remove("POSIXLY_CORRECT")
        .arg("check")
        .args(args)
        .output()
        .expect("failed to spawn rigor binary");
    (
        String::from_utf8(out.stdout).unwrap(),
        String::from_utf8(out.stderr).unwrap(),
        out.status.code().unwrap_or(-1),
    )
}

const FILES: [&str; 3] = ["a.rb", "b.rb", "c,d:e.rb"];

fn check_format(format: &str) -> (String, String, i32) {
    let dir = project();
    let mut args = vec!["--format", format];
    args.extend(FILES);
    check(dir.path(), &args)
}

const TEXT: &str = r#"a.rb:1:7: error: undefined method `lenght' for "abc" [call.undefined-method]
a.rb:2:3: error: undefined method `upcase' for 1 [call.undefined-method]
b.rb:1:5: warning: wrong number of arguments to `upcase' on String (given 3, expected 0..2) [call.wrong-arity]
c,d:e.rb:1:7: error: undefined method `fooo' for "%,:" [call.undefined-method]

3 error(s) in 2 file(s)
"#;

const GITHUB: &str = r#"::error file=a.rb,line=1,col=7,title=call.undefined-method::undefined method `lenght' for "abc"
::error file=a.rb,line=2,col=3,title=call.undefined-method::undefined method `upcase' for 1
::warning file=b.rb,line=1,col=5,title=call.wrong-arity::wrong number of arguments to `upcase' on String (given 3, expected 0..2)
::error file=c%2Cd%3Ae.rb,line=1,col=7,title=call.undefined-method::undefined method `fooo' for "%25,:"
"#;

const SARIF: &str = r#"{
  "version": "2.1.0",
  "$schema": "https://json.schemastore.org/sarif-2.1.0.json",
  "runs": [
    {
      "tool": {
        "driver": {
          "name": "Rigor",
          "informationUri": "https://github.com/rigortype/rigor",
          "version": "{VERSION}",
          "rules": [
            {
              "id": "call.undefined-method"
            },
            {
              "id": "call.wrong-arity"
            }
          ]
        }
      },
      "results": [
        {
          "level": "error",
          "message": {
            "text": "undefined method `lenght' for \"abc\""
          },
          "locations": [
            {
              "physicalLocation": {
                "artifactLocation": {
                  "uri": "a.rb"
                },
                "region": {
                  "startLine": 1,
                  "startColumn": 7
                }
              }
            }
          ],
          "ruleId": "call.undefined-method"
        },
        {
          "level": "error",
          "message": {
            "text": "undefined method `upcase' for 1"
          },
          "locations": [
            {
              "physicalLocation": {
                "artifactLocation": {
                  "uri": "a.rb"
                },
                "region": {
                  "startLine": 2,
                  "startColumn": 3
                }
              }
            }
          ],
          "ruleId": "call.undefined-method"
        },
        {
          "level": "warning",
          "message": {
            "text": "wrong number of arguments to `upcase' on String (given 3, expected 0..2)"
          },
          "locations": [
            {
              "physicalLocation": {
                "artifactLocation": {
                  "uri": "b.rb"
                },
                "region": {
                  "startLine": 1,
                  "startColumn": 5
                }
              }
            }
          ],
          "ruleId": "call.wrong-arity"
        },
        {
          "level": "error",
          "message": {
            "text": "undefined method `fooo' for \"%,:\""
          },
          "locations": [
            {
              "physicalLocation": {
                "artifactLocation": {
                  "uri": "c,d:e.rb"
                },
                "region": {
                  "startLine": 1,
                  "startColumn": 7
                }
              }
            }
          ],
          "ruleId": "call.undefined-method"
        }
      ]
    }
  ]
}
"#;

#[test]
fn text_matches_the_reference() {
    assert_eq!(check_format("text"), (TEXT.to_string(), String::new(), 1));
}

#[test]
fn github_matches_the_reference() {
    assert_eq!(check_format("github"), (GITHUB.to_string(), String::new(), 1));
}

#[test]
fn sarif_matches_the_reference_but_for_the_tool_version() {
    let expected = SARIF.replace("{VERSION}", env!("CARGO_PKG_VERSION"));
    assert_eq!(check_format("sarif"), (expected, String::new(), 1));
}

/// Zero rows: text prints `No diagnostics`, github prints nothing at all, and
/// sarif still prints a full document with empty `rules` and `results`.
#[test]
fn zero_rows_match_the_reference() {
    let dir = project();
    let clean = |format: &str| check(dir.path(), &["--format", format, "clean.rb"]);
    assert_eq!(clean("text"), ("No diagnostics\n".to_string(), String::new(), 0));
    assert_eq!(clean("github"), (String::new(), String::new(), 0));
    let (stdout, stderr, code) = clean("sarif");
    assert_eq!((stderr.as_str(), code), ("", 0));
    assert!(stdout.contains("          \"rules\": []\n"), "{stdout}");
    assert!(stdout.ends_with("      \"results\": []\n    }\n  ]\n}\n"), "{stdout}");
}

/// Warnings only: the run succeeds, so text prints the rows and no summary.
#[test]
fn warning_only_text_has_no_summary() {
    let dir = project();
    assert_eq!(
        check(dir.path(), &["--format", "text", "b.rb"]),
        (
            "b.rb:1:5: warning: wrong number of arguments to `upcase' on String \
             (given 3, expected 0..2) [call.wrong-arity]\n"
                .to_string(),
            String::new(),
            0
        )
    );
}
