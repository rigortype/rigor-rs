# A project class defined ONLY under a namespace whose SHORT name collides with
# a bundled-RBS toplevel class.
#
# `DidYouMean` is a toplevel `module` in the rbs stdlib set, and rspec-core
# defines `RSpec::Core::DidYouMean` (rigor-survey
# `rspec-core-3.13.6/lib/rspec/core/{did_you_mean,configuration}.rb`). The
# reference resolves the bare constant LEXICALLY to the project class, so
# `#call` is defined and it says nothing. rigor-rs's Nominal carries only the
# short key `DidYouMean`, which used to find the stdlib module's (unrelated)
# method table and report `undefined method 'call'`.
#
# The companion `40_unresolved_toplevel_singleton` / `70_nested_shadow_sig`
# fixtures pin the OTHER direction of this short-key asymmetry.

module Harness
  module Inner
    class DidYouMean
      def initialize(name)
        @name = name
      end

      def call
        @name
      end
    end

    class Runner
      def run
        DidYouMean.new("x").call
      end
    end
  end
end

# The bundled stdlib class itself is untouched — a real typo on the toplevel
# `DidYouMean` module still fires, since the project's `DidYouMean` is nested and
# so never suppresses the singleton surface.
::DidYouMean.totally_bogus_name

# `DidYouMean.formatter` / `.correct_error` are NOT typos: they are declared by
# `data/vendored_gem_sigs/did_you_mean/did_you_mean_extras.rbs`, which the
# reference loads in every run and rigor-rs now vendors under
# `crates/rigor-index/vendor/rbs/overlay/` (rigor-survey
# `rake-13.4.2/lib/rake/task_manager.rb:73`). Silence here is the whole point of
# vendoring the overlay.
::DidYouMean.formatter
::DidYouMean.spell_checkers

# Same for the core overlay: `Numeric#to_f` is declared by
# `data/core_overlay/numeric.rbs`, not by upstream RBS.
def harness_widen(a, b)
  (a + b)
end
