# #326: baseline scalars typed through Psych's ScalarScanner

PR #807 → merged 2026-11-22 (`f75eeac6`). Closes #326. Six Opus
adversarial rounds, last **Approved** on `dcc1980c`; 0 FP / sweep;
85-row probe matrix + ~80 review repro rows all `=` on exit/stdout
(crash-shape `rigor: <Class>` stderr vs upstream backtrace is the
accepted convention).

## What landed

`baseline` previously compared raw strings. Now every scalar — top-level
`version:`/`ignored:`/unknown keys, row heads, member fields, nested
seq items, flow collections — resolves through a port of Psych's
`ScalarScanner`/`ToRuby`: YAML 1.1 null/bool/int/float/symbol/token
tokenize + sexagesimal, `deserialize`-style `!ruby/` tag handling
(including `!ruby/encoding` via a generated alias table), quoted-fold
(`'`/`"`/`''`/`\"`) and `\`-suppression, comment stripping, and a
`LoadError::Fatal` split for uncaught upstream exceptions
(`Psych::DisallowedClass`, `Psych::SyntaxError`, `ArgumentError`,
`JSON::GeneratorError` on `dump --format json`).

Row accumulation mirrors `YAML.safe_load`'s whole-document typing:
shape errors (`must be a Hash`, `expected a Hash`, `unsupported
version:`) defer to `parse_loaded` so a later DisallowedClass death or
`version:` check wins, exactly in upstream order. `dump`/`drift`/`prune`
sort with `ruby_sort` (upstream's `Float`-based comparator + Opaque
fallback), `baseline drift` prints `## Cleared` before sorting, and
baseline keys emit in `file`/`rule`/`message`/`count` order with
upstream `class_name` text.

## Deferred gaps (all predate the port's first baseline parser)

- Flow-map row heads (`- {file: a.rb}` → upstream parses a Hash),
  `{version: 1}` flow root, `%YAML` directives, `?`-complex keys.
- `version:1`/`ignored:[]` (no space after colon) → upstream
  SyntaxError, port accepts.
- Duplicate `ignored:` → upstream keeps the LAST, port merges rows.
- `ignored: !str`-style tags on the seq itself; escaped quoted keys
  (`- "fi\x6ce":`); `a: b: c` double colons; malformed flows
  (`[ ,a]`, `[a,,b]`) → upstream SyntaxErrors the port accepts
  (coverage side, no FP).
- Flow-map quoted keys keep quotes in `got` inspects
  (`{"'a: b'" => 1}` vs `{"a: b" => 1}`) — message-only; a fix was
  written but stayed off the merge head under the no-drift rule.
- `baseline drift`/`prune` without `paths:` (pre-existing CLI gap,
  unrelated to parsing).
