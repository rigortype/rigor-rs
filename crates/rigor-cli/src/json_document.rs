//! Reference `CLI::JsonDocument` (v0.4.7, ADR-126 WD2): every `--format=json`
//! document Rigor defines leads with `"schema_version": 1` (then `"mode"`
//! where a flag selects among several documents), and `type-of`, `explain`
//! and `trace` print a one-line stderr notice that their top-level JSON shape
//! changes in the next minor.

/// The document version marker (`JsonDocument::SCHEMA_VERSION`).
pub const SCHEMA_VERSION: u32 = 1;

/// `JsonDocument.stamp` over an already-rendered `JSON.pretty_generate`-shaped
/// object (`{\n  ...`): `schema_version` (then `mode`) becomes the first key.
/// serde_json sorts object keys, so a `json!` key would not lead.
pub fn stamp_pretty(pretty: &str, mode: Option<&str>) -> String {
    let mut head = format!("{{\n  \"schema_version\": {SCHEMA_VERSION}");
    if let Some(m) = mode {
        head.push_str(&format!(",\n  \"mode\": {}", serde_json::to_string(m).unwrap()));
    }
    match pretty.strip_prefix("{\n") {
        Some(rest) => format!("{head},\n{rest}"),
        // `{}` — an empty object pretty-prints on one line.
        None => format!("{head}\n}}"),
    }
}

/// `JsonDocument.shape_notice`.
fn shape_notice(command: &str, replacement: &str) -> String {
    format!(
        "rigor: note — `rigor {command} --format=json` will print {replacement} from the next minor \
         release (currently a different top-level shape); update JSON consumers now. See ADR-126."
    )
}

/// `SHAPE_NOTICES[:type_of]`.
pub fn type_of_notice() -> String {
    shape_notice("type-of", r#"{ "schema_version": 1, "results": [...] }"#)
}

/// `SHAPE_NOTICES[:explain_rules]`.
pub fn explain_rules_notice() -> String {
    shape_notice("explain", r#"{ "schema_version": 1, "rules": [...] }"#)
}

/// `SHAPE_NOTICES[:explain_skip_reasons]`.
#[allow(dead_code)]
pub fn explain_skip_reasons_notice() -> String {
    shape_notice("explain", r#"{ "schema_version": 1, "skip_reasons": [...] }"#)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamp_leads_the_object() {
        assert_eq!(stamp_pretty("{\n  \"a\": 1\n}", None), "{\n  \"schema_version\": 1,\n  \"a\": 1\n}");
        assert_eq!(
            stamp_pretty("{\n  \"a\": 1\n}", Some("precision")),
            "{\n  \"schema_version\": 1,\n  \"mode\": \"precision\",\n  \"a\": 1\n}"
        );
        assert_eq!(stamp_pretty("{}", None), "{\n  \"schema_version\": 1\n}");
    }
}
