# A project `sig/` that references a type it never declares — an INTERFACE
# (`_Writable`) and a type ALIAS (`serialized_node`). See the sibling
# `79_sig_undeclared_type_ref.sig/`.
#
# Rigor stubs a referenced-but-undeclared type so RBS's all-or-nothing per-class
# build still succeeds. Interface and alias names cannot be declared as a
# `class`, so in the PINNED reference one of them makes the whole stub batch
# unparseable and the batch is discarded IN SILENCE — the project's own `sig/`
# then contributes nothing at all, and every diagnostic it would have produced
# disappears. rigor-rs declares each stub in the kind its name requires, so the
# signature survives and `emit_typo` is witnessed.
#
# That makes this an EXCUSED divergence (ADR-0011, `harness/divergence-registry.yml`):
# the reference behaviour is a defect, upstream agrees — rigortype/rigor#237,
# fixed on master by `9515c8f8` + `5bd0aac2`, released after the `v0.3.1` pin.
# **Delete the registry entry when the pin passes that fix**; the two
# implementations agree again from there.
#
# This fixture exists because NO sweep can find this class of bug:
# `harness/fp_audit.py` and `run_corpus.rb` both run each side from a clean cwd
# (core+stdlib only), so project-`sig/` behaviour is invisible to 9204 swept
# files. Only a staged fixture reaches it.

class Report
  def initialize(sink)
    @sink = sink
  end

  def emit(text)
    @sink.write(text)
  end
end

Report.new($stdout).emit_typo("x")
