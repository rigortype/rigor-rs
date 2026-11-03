# Upstream v0.4.2 (rigor#1706 / issue #1697): a top-level `include M` is
# `main.include`, i.e. `Object.include`, so `M`'s instance methods exist on
# every object. With a KNOWN module (a project module, or one the bundled RBS
# declares) the reference silences a bare top-level call AND a call on any
# receiver for a name `M` (or a module `M` itself includes) defines, and keeps
# firing on a name none of them defines. The include counts project-wide, from
# whichever analysed file writes it as a direct program statement; this
# single-file fixture is the same-file half (the cross-file half is the
# rigor-rules test `toplevel_include_reaches_other_files`).
#
# An `include` inside a `def`, a block or a conditional, a `send(:include, M)`
# and a top-level `extend` are not recorded, so their modules answer nothing.
#
# Every row is oracle-measured at the `v0.4.2` pin.

module TkInner
  def tk_inner = 1
end

module TkGreeting
  include TkInner
  def tk_greeting = 1
end

module TkNotIncluded
  def tk_not_included = 1
end

module TkExtended
  def tk_extended = 1
end

include TkGreeting
include Comparable
extend TkExtended
send(:include, TkNotIncluded)

# --- silent: the mixed-in modules define these ---------------------------
tk_greeting
tk_inner
clamp(1, 2)
1.tk_greeting
nil.tk_greeting
"text".tk_inner
Integer.tk_greeting

class TkUser
  def go = tk_greeting
end

# --- MUST STILL FIRE: no mixed-in module defines these --------------------
tk_greetingx
1.tk_greetingx
"text".tk_innerx
tk_not_included
1.tk_not_included
tk_extended
1.tk_extended
