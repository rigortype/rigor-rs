# Issue #157 — config value validation parity

PR #371, merged `0d60b28` (final head `dbd99a2`). Two adversarial rounds.

## What landed

The reference's config validation is now ported end-to-end in `config.rs`:

- Rejection surfaces: `severity_profile`/`severity_overrides` bad values,
  `parallel.workers < 0`, `plugins_isolation`, `bleeding_edge`,
  `plugins_io.network`, plugin-entry shapes — all exit 64 with byte-identical
  stderr; non-mapping `cache`/`dependencies`/`bundler`/`rbs_collection`/
  `plugins_io` keep the exit-1 TypeError crash shape.
- `target_ruby`: `3.3`/`3.3.0`/`4.0`/`latest` run; `3.2`/`4.2`/`33.4`/…
  produce exactly one `configuration-error` row (exit 1); `bogus` → 64.
- Bare plugin ids (`activesupport-core-ext`) are gem names →
  `plugin_loader.load-error` rows instead of silent normalisation.
- Scalar `signature_paths: sig` accepted; missing-dir warning shape matches.
- `ruby_integer` ports Ruby `Integer()` grammar (radix prefixes with empty
  digits rejected — `"0x"` is not 0).
- `enabled:` disables a plugin entry only on literal `false` (`~`/`null`
  keep it — `!= false` upstream).
- `RIGOR_RACTOR_WORKERS`: empty treated as unset; non-Integer values get a
  concise exit-64 (maintainer amendment over upstream's ArgumentError crash).
- `conformance_gate` env check uses the same `Integer()` grammar.

## Review rounds

- **r1**: 3 blockers — `enabled: ~` disabling, `RIGOR_RACTOR_WORKERS=""`
  rejection, bare-radix-prefix `Some(0)` — fixed in `dbd99a2`.
- **r2**: ~80 probes, all fixes byte-identical; every remaining divergence is
  the **#156 YAML-1.1 plain-scalar family** (unfixable at the `Value` layer):
  `enabled: no`/`off`, `bleeding_edge: on`, `workers: 1,000`/`12:34` —
  instance list appended to #156. FP analog noted: `enabled: off` on a real
  plugin keeps it loaded → misses upstream's `blank?` undefined-method; the
  quoted `off`/`"off"` distinction needs the #156 resolver.
- Sweep re-verified by reviewer: 0 FP, gaps = standing baselines.

## Residuals

- **#360** — manual-dispatch missing-name grammar (from #155's review).
- #156 instance list extended (comment on the issue).
- `dependencies.source_inference` warning rows → tracked under #348.
