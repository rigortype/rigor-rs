# Issue #129 / ADR-0044: `%a{rigor:v1:conforms-to _Interface}` in the project's
# own `sig/`. A PROJECT fixture: it ships both a sidecar `.rigor.yml` (which
# CONFIGURES `signature_paths:` — the reference scans the directive only then)
# and a `.sig/` dir, so the harness stages the sidecar as the project's own
# `.rigor.yml` and compares the rows positioned in the staged `sig/*.rbs`
# (the rows are at the ANNOTATION; there is no Ruby `def` to report at).
#
# Every row and every silence in `112_conforms_to_directive.sig/` was measured
# on the reference at the `e59b7b89` pin (one fresh temp cwd per case,
# `--no-cache`); see the comments there. This Ruby file itself is clean.

class Gate
  def close; end
end

Gate.new.close
