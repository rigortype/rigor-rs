# #346: config `~` expansion via `Dir.home`'s NSS chain; `~name` literal

PR [#630](https://github.com/rigortype/rigor-rs/pull/630), merged 2026-11-18.
Two divergences at the config-path `~` surface, both against the v0.4.8
contract (`ConfigPath.absolute`), closed in one PR.

## What was wrong

`expand_tilde_head` ran a hand-rolled passwd model:

- `~`/`~/` with `HOME` unset: `id -u` + an `/etc/passwd` scan. macOS accounts
  exist only in OpenDirectory, so the scan missed every real user and `~`
  stayed literal — `paths: [~/src]` under `env -u HOME` produced a
  `path.not-found` where the reference read the real home.
- `~name`: expanded via the same scan, and an unknown user died
  `rigor: user X doesn't exist` (exit 1). At the pin the reference never
  expands `~name` in config — `HOME_PREFIX = %r{\A~(?=/|\z)}` admits only `~`
  and `~/` (`d1779f95f`, upstream #1510), and `--config` is
  `File.absolute_path`'d (`91c86cac8`). Oracle: `includes:
  [~megurine/inc.yml]` → `include not found: "~megurine/inc.yml"` exit 64
  *even though `megurine` exists*.

The issue brief asked for `~name` via `getpwnam_r` — implementing it
literally would have kept a branch the reference deleted. Spec-vs-oracle
resolved to the oracle; the acceptance criterion ("`~unknownuser/x` matches
the reference's error") holds *because* the reference's answer is the
literal-path error, not a passwd error.

## What the port now does

- `expand_config_entry` ports `ConfigPath.absolute` wholesale: `~`/`~/`
  substitute `dir_home()`, everything else (`~name` included) joins `base`
  literally; infallible — `Dir.home`'s `ArgumentError` rescues to the
  literal spelling upstream.
- `dir_home()` is MRI's `rb_default_home_dir`: `ENV["HOME"]` verbatim
  (empty and relative values included — the "non-absolute home" check is
  `File.expand_path`'s, not `Dir.home`'s), then `getpwnam(getlogin())`
  (POSIX: several login names may share one uid, each with its own passwd
  home — the name wins), then `getpwuid(getuid())`. libc `getpwnam_r` /
  `getpwuid_r`, `cfg(unix)`; non-unix keeps the env-only answer.
- `Config::read` dropped its `~` step: `--config ~/x.yml` reads
  `cwd/~/x.yml` literally, matching `load_with_includes`'s
  `File.absolute_path`.

## Host detail worth remembering

On the dev host, `getlogin()` returns `root`, so HOME-unset `~` resolves to
`/var/root` on **both** engines — that IS the oracle's answer there
(`getpwuid` would give `/Users/megurine`; `Dir.home` does not use it first).
A positive HOME-unset expansion into a real dir is unverifiable on such a
host; the probes prove identical *resolution*.

## Measured outcome

`probe.py --dir` rows (HOME set/unset, `~/`, `~name` existing/unknown,
bare `~`, `HOME=""`/`rel`/`/tmp/`, `pre_eval`, `allowed_paths`,
`signature_paths`, `--config` literal `~/` + decoy): all identical except
the pre-existing `[path.not-found]` human-format bracket (fires on any
nonexistent path — no `~` involved). Two rounds of review: Opus 5.5 and
Grok 4.6 both `Approved`; Fable 5.2 design companion, no blocking findings.

## Follow-ups filed

- **#631** — remaining `ConfigPath.absolute` surfaces (`rbs_collection`/
  `bundler` lockfiles, `bundle_path`, `.bundle/config BUNDLE_PATH`, config
  audit, `bundle_sources_present`'s HOME-unset decline); NSS hardening
  nits (`pw_dir` null guard, `getlogin_r`); an unverified `core_root` exit
  report (did not reproduce in three shapes).
- **#632** — `expand_path` folds a leading `//` `File.absolute_path` keeps
  (`HOME=/` + `~/x`; also plain `//tmp/x`). Message drift only.
- **#633** — `includes:` a directory → reference uncaught `Errno::EISDIR`
  backtrace vs the port's clean error (exit 1 both; reference defect
  territory).
