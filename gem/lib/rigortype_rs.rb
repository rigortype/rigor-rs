# frozen_string_literal: true

require "rigortype_rs/version"

# `rigortype-rs` is the precompiled-binary distribution of the Rust port of
# Rigor (ADR-0010's PRIMARY channel). The gem is a thin shim: it bundles the
# native `rigor` binary at `libexec/rigor` and `exe/rigor` exec-dispatches to
# it. See `RigortypeRs::Binary` for the resolution + the not-found guidance.
module RigortypeRs
end
