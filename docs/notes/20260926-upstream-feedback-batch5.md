# Upstream feedback, batch 5: reference failures found in #129 follow-up triage (2026-09-26)

Batches 1–4: [20260716](20260716-upstream-feedback.md), [20260807](20260807-upstream-feedback-batch2.md),
[20260826](20260826-upstream-feedback-batch3.md), [20260909](20260909-upstream-feedback-batch4.md).

Measured against the PINNED reference `e59b7b89`, from a fresh temp cwd per run, with
`--no-cache` and both `-I` libs (`UPSTREAM.md` hazard 1). Host: Ruby 4.0.6, rbs 4.2.0.

The port does **not** reproduce any of these (maintainer decision, 2026-09-26). AGENTS.md
does not port transient reference defects. The rigor-rs issues track following
whatever upstream settles on.

> **Status: DRAFTS, not filed.** Items 1–3 go to rigortype/rigor; item 4 goes to ruby/rbs.
> After filing, record the upstream numbers here and on the tracking issues.

## 1. A NUL byte in a project `.rbs` becomes an internal analyzer error on every target (tracking: #160)

```sh
mkdir -p sig lib
printf 'class A\n  def foo: () -> Integer\nend\n\0\n' > sig/a.rbs
printf 'A.new.foo\n' > lib/a.rb
rigor check --no-cache lib/a.rb
```

```
lib/a.rb:1:1: error: internal analyzer error: ArgumentError: string contains null byte (lib/rigor/environment/rbs_loader.rb:550:in 'Rigor::Environment::RbsLoader.parse_signature_file')
```

Exit 1. The JSON row has `rule: null`. The error is attributed to the Ruby target, not
to the signature file, and every target reports it. We'd expect the loader to treat
the file like any other unparseable signature file: a load diagnostic on the `.rbs`,
with the rest of the environment intact.

## 2. `use Nope::*` (a namespace that does not exist) raises `KeyError` in the loader (tracking: #160)

```sh
mkdir -p sig lib
printf 'use Nope::*\nclass A\n  def foo: () -> Integer\nend\n' > sig/a.rbs
printf 'A.new.foo\n' > lib/a.rb
rigor check --no-cache lib/a.rb
```

```
lib/a.rb:1:1: error: internal analyzer error: KeyError: key not found: #<RBS::Namespace … @path=[:Nope] …> (lib/rigor/environment/rbs_loader.rb:254:in 'block in Rigor::Environment::RbsLoader.resolve_quarantining_virtual_collisions')
```

Exit 1, rule `null`. The #129 audit traced the raise to `UseMap#build_map`. We'd expect
a diagnostic on the `.rbs` for the unresolvable clause, with the rest of the file loaded.

## 3. A non-integer `RIGOR_RACTOR_WORKERS` crashes the process (tracking: #157)

```sh
printf '1\n' > a.rb
RIGOR_RACTOR_WORKERS=abc rigor check --no-cache a.rb
```

```
lib/rigor/cli/check_runner_factory.rb:50:in 'Kernel#Integer': invalid value for Integer(): "abc" (ArgumentError)
	from lib/rigor/cli/check_runner_factory.rb:50:in 'Rigor::CLI::CheckRunnerFactory.resolve_workers'
	from lib/rigor/cli/check_runner_factory.rb:33:in 'Rigor::CLI::CheckRunnerFactory.build'
```

Exit 1, with a backtrace on stderr. `2x` and `1.5` crash the same way. The equivalent
flag is validated: `--workers=abc` gives `invalid argument: --workers=abc`, exit 64.
We'd expect the environment variable to be rejected the same way, as a usage error with
exit 64. rigor-rs is adopting exactly that (#157), so a matching upstream fix would make
the two agree.

## 4. ruby/rbs: publish a `ruby-rbs-sys` / `ruby-rbs` built on rbs ≥ 4.2 (tracking: #169)

The latest crate on crates.io, `ruby-rbs-sys` 0.3.0, was published from `ca09597`. It
vendors the **v4.0.2** C parser, whose lexer is ASCII-only (`word = [a-zA-Z0-9_]`). So it
rejects the non-ASCII method, parameter and ivar names that ruby/rbs#3082 (in 4.2.0)
made valid, e.g. `def été: () -> void`, and a Rust consumer drops the whole file. On
master, `rust/rbs_version` is v4.1.2, which still predates #3082. A `git` dependency is
not an option, because `vendor/rbs/` is only committed on release branches.

Ask: re-pin the Rust crates to v4.2.x (`rake rust:rbs:pin[v4.2.0]`) and publish. Until
then, rigor-rs carries a vendored 0.3.0 with #3082 backported (#169). The crate release
is that fork's exit condition.
