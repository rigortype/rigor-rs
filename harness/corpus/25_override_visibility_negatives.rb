# def.override-visibility-reduced — adversarial NEGATIVES that must stay SILENT
# on BOTH tools (zero-FP discipline):
#
#   * Widening (private parent -> public override) is NOT a reduction.
#   * An RBS / third-party ancestor (ApplicationRecord) is not a project source
#     class, so no project ancestor defines the method -> silent.
#   * `private def foo` (the modifier-wrapping-a-def form) is not tracked as a
#     visibility change -> records at the running default (public) -> silent.

# Widening: the override is WIDER, never flagged.
class Widener
  private

  def w; end
end

class WidenerChild < Widener
  def w; end
end

# RBS / third-party ancestor: ApplicationRecord is neither source nor walked,
# so even a strict-looking private override has no project ancestor to compare.
class Account < ApplicationRecord
  private

  def save_it; end
end

# `private def` form: untracked, records at public, so no reduction is observed.
class ModifierForm
  def base; end
end

class ModifierFormChild < ModifierForm
  private def base; end
end
