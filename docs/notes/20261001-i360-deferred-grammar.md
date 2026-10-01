# Issue #360 — deferred-command missing-name grammar

PR #375, merged `a1ee896` (head `c6a9227`). Approved after ~65-probe
adversarial review — every error-grammar row byte-identical on stdout, stderr
and exit code.

## What landed

`crates/rigor-cli/src/main.rs` (+127/−21):

- `cmd_plugin_deferred`: `path`/`print` split out of the deferred arm. Missing
  argv[1] → `` `<verb>` requires a plugin name `` + `PLUGIN_USAGE` on stderr,
  exit 64. Unresolved name → `Unknown plugin: X` + bundled-list hint, exit 1.
- `cmd_skill_deferred`: `--full`/`--path`/`--print` split from `--list`;
  missing name → per-flag `usage_error` (`--print` shares `run_print`'s
  positional message — verified ref lines 124/141/161) + `SKILL_USAGE`, exit 64.
- `plugin_name_known` + `BUNDLED_PLUGIN_NAMES` (39 production + 6 example at
  pin e59b7b89) reproduce `PluginCommand#find`: exact match or `rigor-` prefix
  dropped from either side. `BUNDLED_SKILL_NAMES` (17, all with `SKILL.md`) is
  exact-match only — `find_skill` does no prefix stripping, and the port's
  `contains` preserves that asymmetry.

## Oracle semantics confirmed by probing

- The name slot swallows dashed tokens: `plugin path --bogus` → `Unknown
  plugin: --bogus` (exit 1), not an option error.
- `plugin path --help` → `Unknown plugin: --help` — flags are names once past
  the verb.
- Extra argv after name/subcommand is ignored.
- Ref `find` resolves only against `PLUGINS_ROOT`/`EXAMPLES_ROOT` — no
  Gemfile/installed-gem lookup — so a hardcoded catalogue is the correct
  mechanism for the error surface.

## Deferred by design (disclosed coverage)

`plugin`/`list`/`root`, `plugin path|print <known>`, `skill`/`--list`,
`skill <name>`/`--full|--path|--print <name>`, `describe` — resolved bodies
stay exit-2 stubs; printing them would require shipping the plugin/skill
trees.

## Residuals

- **#376** — `BUNDLED_PLUGIN_NAMES`/`BUNDLED_SKILL_NAMES` are pin-coupled
  transcriptions with no mechanical tie to the submodule; a pin bump that
  adds/removes a dir silently desyncs the error surface.
