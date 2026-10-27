# Deep nesting: where the reference aborts and the port reports (#412)

[ADR-0045](../adr/0045-stack-totality.md) makes the port total on deep
nesting: it never overflows its stack. The reference does not survive the same
inputs. It catches its `SystemStackError` in `cli.rb`'s `rescue *FATAL_ERRORS`
and aborts the whole run:
- stderr: `rigor: analysis aborted: SystemStackError: stack level too deep`,
  then the "report is EMPTY rather than clean … This is a defect in Rigor"
  lines;
- stdout: empty, for `--format json` too;
- exit 70.

The depth at which it aborts depends on Ruby's VM stack size
(`RUBY_THREAD_VM_STACK_SIZE`), the Ruby version, and which walker it reaches
first. Upstream master already aborts at about 700 nested `if`s and about
1,300 nested arrays (rigortype/rigor#2455). There is no portable boundary to
mirror, so the port does not mirror one.

**The divergence.** Where the reference aborts and the port finishes, the port's
report is a reference defect, tracked upstream. It is not a false positive to
suppress. Above the port's own bound (25,000 Prism child edges), the port
reports an `internal-error` `:info` finding for that file and keeps analysing
the rest of the run.

## Probe tables

Both engines, from a fresh cwd. The reference is at pin `e59b7b89`, on the
system Ruby's default VM stack, with `--no-cache`.

**Triage probe from #412, port at master `c7c5a72`:**

| file | reference | port, 1 file | port, + 1 other file |
|---|---|---|---|
| `[[[…]]]` ×500 / ×1500 | ok | ok | ok |
| `[[[…]]]` ×3000 | exit 70 | ok, no diagnostics | – |
| `[[[…]]]` ×9999 | exit 70 | SIGABRT, exit 134 | – |
| `if a` ×500 | 500 × `call.unresolved-toplevel` | the same 500 | – |
| `if a` ×1500 / ×3332 | exit 70 | 1500 / 3332 warnings, exit 0 | SIGABRT (×1500) |
| `x.succ` chain ×1500 / ×5000 | exit 70 | ok, no diagnostics | ok (×1500) |

**Re-probe with `harness/probe.py -f`, port with #412, one file:**

| file | reference | port |
|---|---|---|
| `[[[…]]]` ×500 / ×1500 | match: silent, exit 0 | match |
| `[[[…]]]` ×3000 / ×9999 | exit 70 | silent, exit 0 |
| `if x` ×500 / ×700 | match: one `call.unresolved-toplevel` per level | match |
| `if x` ×1500 / ×3332 | exit 70 | one warning per level, exit 0 |
| `x.a` chain ×700 | match: one `call.unresolved-toplevel`, exit 0 | match |
| `x.a` chain ×1500 / ×5000 | exit 70 | one warning, exit 0 |
| `x.a` chain ×30000 | exit 70 | `internal-error` `:info` ("source nesting depth 30003 exceeds the analysis limit of 25000 …"), exit 1 |

At the pin, the reference finished 700 nested `if`s and a 700-link chain; it
aborted from 1,500. The two-file runs that crashed the port on master
(SIGABRT on a 2 MiB rayon worker) now finish too. The release acceptance
covers every construct at Prism's cap, on one file and on two.
