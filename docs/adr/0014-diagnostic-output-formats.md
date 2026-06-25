# Emit the reference's machine-readable formats with identical schema

Status: accepted

rigor-rs models a diagnostic as a structured value — a rule id, severity, primary span, secondary `annotation`s, and `subdiagnostic`s (the ruff_db / miette shape, [ADR-0005](0005-rust-architecture.md)) — and renders the reference's full set of output formats: human text, JSON, SARIF, GitHub annotations, GitLab Code Quality, Checkstyle, JUnit, TeamCity.

The **machine-readable formats match the reference's schema and field names** (rule id, location, severity), so existing CI integrations, dashboards, and review-bot consumers keep working unchanged under a drop-in replacement ([ADR-0001](0001-rust-reimplementation-strategy.md) / [ADR-0009](0009-config-baseline-compatibility.md)). Only the **human text format's wording** may improve, within the latitude of [ADR-0002](0002-diagnostic-set-parity.md). The machine formats carry the rule id + location that diagnostic-set parity is defined over, so their structure is parity-bearing and held stable.

## Considered options

- **Ship a core subset (human/JSON/SARIF) now, defer the rest** — rejected as the contract: leaves GitLab/Checkstyle/JUnit/TeamCity integrations broken on switch; the formats are cheap renderers over one structured model, so all are provided.
- **A bespoke rigor-rs output as the primary format** — rejected: breaks existing tooling consumers, friction against drop-in replacement.
