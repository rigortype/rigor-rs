# 2026-11-12 — #349 Psych YAML error fidelity

PR #576 merged as `e181d98`. `.rigor.yml` parse failures now re-run
serde_yaml's own unsafe-libyaml parser and read `problem`, `context` and
`context_mark + 1` from libyaml, matching Psych's `make_exception` for
`...`, bad `\x`/`\u` escapes (`hexdecimal`), flow sequences, tabs,
directives, control characters, anchors, unclosed quotes and included
files. The detail joins problem/context with one space; a leading UTF-8
BOM is stripped as `r:bom|utf-8` does.

Gates: `harness/gate.sh` pass; pinned 1.88 clippy clean in a fresh
`CARGO_TARGET_DIR`; release `fp_audit.py --gaps --sweep` reported 0 FP;
CI green on merge-commit head `8e1e149`. Review: Opus 5.5 `Approved`;
Fable companion `approve`.

Follow-ups filed: #579 (invalid-UTF-8 reader detail), #580 (closed deep
nesting vs serde_yaml recursion limit). #156/#347 still track full
Psych/alias semantics.
