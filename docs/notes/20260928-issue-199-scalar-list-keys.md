# #199 — scalar list keys (`Array()` semantics)

PR #281 (merged). Config list keys previously deserialized as `Vec<String>`, so a
scalar made the whole document malformed and `Config::default()` silently dropped
every other key — a live FP (`paths: other` fired `call.undefined-method` where
the reference is silent).

One shared `de_ruby_array` deserializer now mirrors `Array(x).map(&:to_s)` on
`disable`, `exclude`, `paths`, `plugins`, `signature_paths`: scalar → one-element
list, null → `[]`, `1`/`true` → `"1"`/`"true"`, mappings and nested collections
still malformed. `signature_paths: ~` is special-cased (reference keeps it `nil`
→ default discovery) via a `signature_paths_null` flag so
`explicit_signature_paths()` returns `None`. `present_keys` /
`paths_explicitly_declared` needed no change — they already record any key.

`conformance_gate::config_text_ok` accepts a string scalar for those keys so a
scalar `signature_paths: sig` runs the same conforms-to scan as the list form
(8 identical rows). This satisfies #157's scalar-`signature_paths` criterion
(PR says `Refs #157`; the bare-plugin-id half stays open there).

Measured: `harness/gate.sh` 0, CI green on `a01c45d`, `fp_audit --gaps --sweep`
0 FP / 9,337. Review: OpenCode primary (GLM-5.2) PASS, final gate (DeepSeek V4
Pro) MERGE_APPROVED with per-key parity table vs `configuration.rb:503-515`.

Residuals (coverage loss, no FP): a mapping or nested-collection value under a
list key is still malformed where the reference `Array()`-accepts it
(`paths: {a: 1}` → `[["[:a, 1]"]]`). `Config::read` parses the YAML text three
times; harmless at config sizes.
