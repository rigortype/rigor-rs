//! CI-native diagnostic output formats (reference ADR-51), ported from
//! `lib/rigor/cli/diagnostic_formats.rs`. Each renders the analyzed diagnostics
//! to a string a CI platform consumes to surface findings inline in a pull /
//! merge request, rather than only in the job log. They read the same fields as
//! `--format json` (path / line / column / severity / qualified rule / message)
//! and add no new information — only a platform-native rendering of it.
//!
//!   gitlab     — GitLab Code Quality report JSON (the CodeClimate subset)
//!                that drives the merge-request Code Quality widget
//!   checkstyle — Checkstyle XML, the broad lint-interchange format that
//!                reviewdog (`-f=checkstyle`) and Jenkins/etc. consume
//!   junit      — JUnit XML, the test-report format many CI systems render
//!   teamcity   — TeamCity inspection service messages (`##teamcity[…]`)
//!
//! ADDITIVE / NOT harness-gated: these never touch the text/json output the
//! differential harness (`harness/run.rb`) depends on. Severity maps once per
//! format from Error / Warning / Info; the qualified rule (here the
//! `rule_id`, which already carries the `builtin` family bare) is the stable
//! identifier surfaced where the format has a slot for it.
//!
//! Each renderer takes a slice of `Rendered` rows — the diagnostic fields after
//! `line_col` has resolved the byte offset to a 1-based (line, column) — so the
//! formatters stay pure and independently testable, mirroring the reference's
//! `Diagnostic`-reading formatter classes.

use rigor_rules::Severity;
use serde::Serialize;

/// One diagnostic flattened for the CI formatters: the resolved location plus
/// the fields a format renders. `rule_id` is the qualified rule (rigor-rs keeps
/// the `builtin` family bare in `rule_id`, so it already equals the reference's
/// `qualified_rule` for built-in rules).
pub struct Rendered<'a> {
    pub path: &'a str,
    pub line: usize,
    pub column: usize,
    pub severity: Severity,
    pub rule_id: &'a str,
    pub message: &'a str,
}

/// GitLab Code Quality report — the CodeClimate-subset JSON array GitLab reads
/// from a `codequality` CI artifact to populate the merge-request Code Quality
/// widget. Severity maps Error→major, Warning→minor, Info→info (the reference's
/// `SEVERITIES`). The rule id is folded into the description (the widget has no
/// dedicated rule field) and a stable SHA-256 `fingerprint` over the locating
/// tuple lets GitLab dedup / track findings across runs.
pub fn render_gitlab(rows: &[Rendered]) -> String {
    // Serde-derived structs so the JSON key order matches the reference's
    // insertion order exactly (description, check_name, fingerprint, severity,
    // location{path, lines{begin}}) — `serde_json::Value` would re-sort keys
    // alphabetically, diverging from the reference's `JSON.pretty_generate`.
    #[derive(Serialize)]
    struct Lines {
        begin: usize,
    }
    #[derive(Serialize)]
    struct Location<'a> {
        path: &'a str,
        lines: Lines,
    }
    #[derive(Serialize)]
    struct Entry<'a> {
        description: String,
        check_name: &'a str,
        fingerprint: String,
        severity: &'a str,
        location: Location<'a>,
    }

    let entries: Vec<Entry> = rows
        .iter()
        .map(|r| Entry {
            description: gitlab_description(r),
            check_name: r.rule_id,
            fingerprint: gitlab_fingerprint(r),
            severity: gitlab_severity(r.severity),
            location: Location {
                path: r.path,
                lines: Lines { begin: r.line },
            },
        })
        .collect();
    serde_json::to_string_pretty(&entries).unwrap()
}

fn gitlab_severity(severity: Severity) -> &'static str {
    match severity {
        Severity::Error => "major",
        Severity::Warning => "minor",
        Severity::Info => "info",
    }
}

/// `"<message> [<rule>]"` — the rule rides in the description because Code
/// Quality has no dedicated rule field (matches the reference).
fn gitlab_description(r: &Rendered) -> String {
    format!("{} [{}]", r.message, r.rule_id)
}

/// SHA-256 over the locating tuple (path, rule, line, column, message) joined by
/// a NUL byte — stable for an unchanged finding and unique per finding, exactly
/// as the reference computes it (`[...].join("\0")`). The NUL separator (not a
/// space) is load-bearing for byte-identical fingerprints with the reference.
fn gitlab_fingerprint(r: &Rendered) -> String {
    let payload = format!(
        "{}\0{}\0{}\0{}\0{}",
        r.path, r.rule_id, r.line, r.column, r.message
    );
    sha256_hex(payload.as_bytes())
}

/// Checkstyle XML — the lint-interchange format reviewdog (`-f=checkstyle`) and
/// Jenkins/etc. read. Errors are grouped by file (in first-appearance order, as
/// the reference's `group_by` preserves); the qualified rule rides in `source`.
/// Checkstyle's native severities are `error`/`warning`/`info`, so rigor-rs's
/// map through unchanged.
pub fn render_checkstyle(rows: &[Rendered]) -> String {
    let mut lines = vec![
        r#"<?xml version="1.0" encoding="UTF-8"?>"#.to_string(),
        "<checkstyle>".to_string(),
    ];
    for (path, group) in group_by_path(rows) {
        lines.push(format!("  <file name=\"{}\">", xml_escape(path)));
        for r in group {
            lines.push(format!(
                "    <error line=\"{}\" column=\"{}\" severity=\"{}\" message=\"{}\" source=\"{}\" />",
                r.line,
                r.column,
                checkstyle_severity(r.severity),
                xml_escape(r.message),
                xml_escape(r.rule_id),
            ));
        }
        lines.push("  </file>".to_string());
    }
    lines.push("</checkstyle>".to_string());
    lines.join("\n")
}

fn checkstyle_severity(severity: Severity) -> &'static str {
    match severity {
        Severity::Error => "error",
        Severity::Warning => "warning",
        Severity::Info => "info",
    }
}

/// JUnit XML — the test-report format GitHub's test reporting, GitLab, Jenkins,
/// and CircleCI render. Following the established linter convention, every
/// diagnostic is a `testcase` carrying a `failure` typed by its severity; a
/// clean run reports one passing case (JUnit wants at least one test). The
/// `classname` is the qualified rule; the `name` is `path:line:column`.
pub fn render_junit(rows: &[Rendered]) -> String {
    let tests = if rows.is_empty() { 1 } else { rows.len() };
    let mut lines = vec![
        r#"<?xml version="1.0" encoding="UTF-8"?>"#.to_string(),
        format!(
            "<testsuite name=\"rigor\" tests=\"{}\" failures=\"{}\">",
            tests,
            rows.len()
        ),
    ];
    if rows.is_empty() {
        lines.push(r#"  <testcase name="rigor" />"#.to_string());
    } else {
        for r in rows {
            let name = format!("{}:{}:{}", r.path, r.line, r.column);
            lines.push(format!(
                "  <testcase name=\"{}\" classname=\"{}\">",
                xml_escape(&name),
                xml_escape(r.rule_id),
            ));
            lines.push(format!(
                "    <failure type=\"{}\" message=\"{}\" />",
                junit_severity(r.severity),
                xml_escape(r.message),
            ));
            lines.push("  </testcase>".to_string());
        }
    }
    lines.push("</testsuite>".to_string());
    lines.join("\n")
}

fn junit_severity(severity: Severity) -> &'static str {
    match severity {
        Severity::Error => "error",
        Severity::Warning => "warning",
        Severity::Info => "info",
    }
}

/// TeamCity inspection service messages — the `##teamcity[…]` lines a TeamCity
/// build agent parses out of the build log into its Inspections view. One
/// `inspectionType` declares the category; each diagnostic is an `inspection`
/// typed by severity. Empty when there are no diagnostics (the reference returns
/// `""`), so a clean run stays quiet.
pub fn render_teamcity(rows: &[Rendered]) -> String {
    if rows.is_empty() {
        return String::new();
    }
    let mut lines = vec![teamcity_message(
        "inspectionType",
        &[
            ("id", "rigor"),
            ("name", "rigor"),
            ("category", "rigor"),
            ("description", "Rigor inspection"),
        ],
    )];
    for r in rows {
        let text = format!("{} [{}]", r.message, r.rule_id);
        let line = r.line.to_string();
        lines.push(teamcity_message(
            "inspection",
            &[
                ("typeId", "rigor"),
                ("message", &text),
                ("file", r.path),
                ("line", &line),
                ("SEVERITY", teamcity_severity(r.severity)),
            ],
        ));
    }
    lines.join("\n")
}

fn teamcity_severity(severity: Severity) -> &'static str {
    match severity {
        Severity::Error => "ERROR",
        Severity::Warning => "WARNING",
        Severity::Info => "INFO",
    }
}

fn teamcity_message(name: &str, attrs: &[(&str, &str)]) -> String {
    let pairs: Vec<String> = attrs
        .iter()
        .map(|(k, v)| format!("{}='{}'", k, teamcity_escape(v)))
        .collect();
    format!("##teamcity[{} {}]", name, pairs.join(" "))
}

/// TeamCity's documented service-message escaping: `|` doubles, then `'`, `\n`,
/// `\r`, `[`, `]` each gain a leading `|`.
fn teamcity_escape(value: &str) -> String {
    value
        .replace('|', "||")
        .replace('\'', "|'")
        .replace('\n', "|n")
        .replace('\r', "|r")
        .replace('[', "|[")
        .replace(']', "|]")
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Group rows by path in first-appearance order (mirrors Ruby `group_by`, which
/// keeps insertion order of the keys).
fn group_by_path<'a, 'b>(rows: &'b [Rendered<'a>]) -> Vec<(&'a str, Vec<&'b Rendered<'a>>)> {
    let mut order: Vec<&'a str> = Vec::new();
    let mut groups: Vec<(&'a str, Vec<&'b Rendered<'a>>)> = Vec::new();
    for r in rows {
        if let Some(idx) = order.iter().position(|p| *p == r.path) {
            groups[idx].1.push(r);
        } else {
            order.push(r.path);
            groups.push((r.path, vec![r]));
        }
    }
    groups
}

/// Escape the five predefined XML entities so a diagnostic message carrying
/// `<`, `&`, or a quote cannot break the document (matches the reference's
/// `XmlEscaping`).
fn xml_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// SHA-256 (pure, dependency-free) for the GitLab Code Quality fingerprint.
// ---------------------------------------------------------------------------

/// SHA-256 of `data`, lowercase hex. A small, self-contained implementation so
/// the GitLab fingerprint matches `Digest::SHA256.hexdigest` byte-for-byte
/// without pulling a crypto crate in for one hash.
fn sha256_hex(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];

    // Pre-processing: append 0x80, pad with zeros, then the 64-bit bit length.
    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in msg.chunks_exact(64) {
        let mut w = [0u32; 64];
        for (i, word) in w.iter_mut().enumerate().take(16) {
            let j = i * 4;
            *word = u32::from_be_bytes([chunk[j], chunk[j + 1], chunk[j + 2], chunk[j + 3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let mut a = h[0];
        let mut b = h[1];
        let mut c = h[2];
        let mut d = h[3];
        let mut e = h[4];
        let mut f = h[5];
        let mut g = h[6];
        let mut hh = h[7];

        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }

        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }

    let mut hex = String::with_capacity(64);
    for word in h {
        hex.push_str(&format!("{word:08x}"));
    }
    hex
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row<'a>(
        path: &'a str,
        line: usize,
        column: usize,
        severity: Severity,
        rule_id: &'a str,
        message: &'a str,
    ) -> Rendered<'a> {
        Rendered { path, line, column, severity, rule_id, message }
    }

    #[test]
    fn sha256_known_vectors() {
        // Standard NIST vectors.
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn gitlab_fingerprint_matches_reference_oracle() {
        // The reference (Digest::SHA256.hexdigest) over the same locating tuple
        // for `sample.rb:2:3 call.undefined-method 'undefined method ...'`.
        let r = row(
            "sample.rb",
            2,
            3,
            Severity::Error,
            "call.undefined-method",
            "undefined method `lenght' for \"Hello\"",
        );
        assert_eq!(
            gitlab_fingerprint(&r),
            "71429464f681723aa0bb5d5d5b781e803baa96c5e1ba14ee55aaac32bd5e5acb"
        );
    }

    #[test]
    fn gitlab_schema_shape() {
        let r = row(
            "sample.rb",
            2,
            3,
            Severity::Error,
            "call.undefined-method",
            "undefined method `lenght' for \"Hello\"",
        );
        let out = render_gitlab(&[r]);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        let e = &v[0];
        assert_eq!(
            e["description"],
            "undefined method `lenght' for \"Hello\" [call.undefined-method]"
        );
        assert_eq!(e["check_name"], "call.undefined-method");
        assert_eq!(e["severity"], "major");
        assert_eq!(e["location"]["path"], "sample.rb");
        assert_eq!(e["location"]["lines"]["begin"], 2);
        assert_eq!(
            e["fingerprint"],
            "71429464f681723aa0bb5d5d5b781e803baa96c5e1ba14ee55aaac32bd5e5acb"
        );
    }

    #[test]
    fn gitlab_empty_is_empty_array() {
        assert_eq!(render_gitlab(&[]), "[]");
    }

    #[test]
    fn gitlab_severity_mapping() {
        assert_eq!(gitlab_severity(Severity::Error), "major");
        assert_eq!(gitlab_severity(Severity::Warning), "minor");
        assert_eq!(gitlab_severity(Severity::Info), "info");
    }

    #[test]
    fn checkstyle_matches_reference_oracle() {
        let r = row(
            "sample.rb",
            2,
            3,
            Severity::Error,
            "call.undefined-method",
            "undefined method `lenght' for \"Hello\"",
        );
        let expected = [
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>",
            "<checkstyle>",
            "  <file name=\"sample.rb\">",
            "    <error line=\"2\" column=\"3\" severity=\"error\" message=\"undefined method `lenght&apos; for &quot;Hello&quot;\" source=\"call.undefined-method\" />",
            "  </file>",
            "</checkstyle>",
        ]
        .join("\n");
        assert_eq!(render_checkstyle(&[r]), expected);
    }

    #[test]
    fn checkstyle_groups_by_file_and_empty() {
        // Two files; each group has its own <file> block in first-appearance order.
        let rows = vec![
            row("a.rb", 1, 1, Severity::Error, "r", "m1"),
            row("b.rb", 2, 2, Severity::Warning, "r", "m2"),
            row("a.rb", 3, 3, Severity::Info, "r", "m3"),
        ];
        let out = render_checkstyle(&rows);
        // a.rb appears once, before b.rb, and carries two errors.
        assert_eq!(out.matches("<file name=\"a.rb\">").count(), 1);
        assert!(out.find("a.rb").unwrap() < out.find("b.rb").unwrap());
        assert_eq!(out.matches("<error ").count(), 3);

        // Empty run: header + close, no <file>.
        let empty = render_checkstyle(&[]);
        assert_eq!(
            empty,
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<checkstyle>\n</checkstyle>"
        );
    }

    #[test]
    fn junit_matches_reference_oracle() {
        let r = row(
            "sample.rb",
            2,
            3,
            Severity::Error,
            "call.undefined-method",
            "undefined method `lenght' for \"Hello\"",
        );
        let expected = [
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>",
            "<testsuite name=\"rigor\" tests=\"1\" failures=\"1\">",
            "  <testcase name=\"sample.rb:2:3\" classname=\"call.undefined-method\">",
            "    <failure type=\"error\" message=\"undefined method `lenght&apos; for &quot;Hello&quot;\" />",
            "  </testcase>",
            "</testsuite>",
        ]
        .join("\n");
        assert_eq!(render_junit(&[r]), expected);
    }

    #[test]
    fn junit_empty_reports_one_passing_case() {
        let expected = [
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>",
            "<testsuite name=\"rigor\" tests=\"1\" failures=\"0\">",
            "  <testcase name=\"rigor\" />",
            "</testsuite>",
        ]
        .join("\n");
        assert_eq!(render_junit(&[]), expected);
    }

    #[test]
    fn teamcity_matches_reference_oracle() {
        let r = row(
            "sample.rb",
            2,
            3,
            Severity::Error,
            "call.undefined-method",
            "undefined method `lenght' for \"Hello\"",
        );
        let expected = "##teamcity[inspectionType id='rigor' name='rigor' category='rigor' description='Rigor inspection']\n\
##teamcity[inspection typeId='rigor' message='undefined method `lenght|' for \"Hello\" |[call.undefined-method|]' file='sample.rb' line='2' SEVERITY='ERROR']";
        assert_eq!(render_teamcity(&[r]), expected);
    }

    #[test]
    fn teamcity_empty_is_empty_string() {
        assert_eq!(render_teamcity(&[]), "");
    }

    #[test]
    fn teamcity_escaping() {
        // | doubles; ' [ ] each gain a leading |; newline -> |n.
        assert_eq!(teamcity_escape("a|b'c[d]e\nf"), "a||b|'c|[d|]e|nf");
    }

    #[test]
    fn xml_escape_all_entities() {
        assert_eq!(xml_escape("a&b<c>d\"e'f"), "a&amp;b&lt;c&gt;d&quot;e&apos;f");
    }
}
