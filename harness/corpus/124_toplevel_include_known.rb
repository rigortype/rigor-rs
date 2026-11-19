# Upstream v0.4.2 (rigor#1706 / issue #1697): a top-level `include M` is
# `main.include`, i.e. `Object.include`, so `M`'s instance methods exist on
# every object. With a KNOWN module (a project module, or one the bundled RBS
# declares) the reference silences a bare top-level call AND a call on any
# receiver for a name `M` (or a module `M` itself includes) defines, and keeps
# firing on a name none of them defines. The include counts project-wide; this
# single-file fixture is the same-file half (the cross-file half is the
# rigor-rules test `toplevel_include_reaches_other_files`).
#
# Upstream v0.4.8 (#2995 / #3004 / #3007) widened the read: `self.include M`,
# the `send` / `__send__` / `self.send` spellings (never `public_send` —
# `main`'s `include` is private), an `include` under top-level control flow,
# in a top-level `def` or `def self.` body, in a value position, and in a
# self-keeping iterator's block on a literal receiver all count as
# `main.include`. An `include` in another block or lambda, in an iterator
# block on a non-literal receiver, and a top-level `extend` do not.
#
# Rows 39–60 are oracle-measured at the `v0.4.2` pin, the rest at `v0.4.8`.

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

module TkPublicSent
  def tk_public_sent = 1
end

module TkUnderIf
  def tk_under_if = 1
end

module TkInDef
  def tk_in_def = 1
end

module TkInSDef
  def tk_in_sdef = 1
end

module TkInValue
  def tk_in_value = 1
end

module TkInEach
  def tk_in_each = 1
end

module TkInMap
  def tk_in_map = 1
end

module TkVarEach
  def tk_var_each = 1
end

module TkInLambda
  def tk_in_lambda = 1
end

include TkGreeting
include Comparable
extend TkExtended
send(:include, TkNotIncluded)
self.include TkGreeting
__send__(:include, TkInner)
self.send(:include, TkGreeting)
public_send(:include, TkPublicSent)
if true
  include TkUnderIf
end
def tk_def_include
  include TkInDef
end
def self.tk_sdef_include
  include TkInSDef
end
x = (include TkInValue)
[1].each { include TkInEach }
[1].map { include TkInMap }
xs = [1]
xs.each { include TkVarEach }
-> { include TkInLambda }

# --- silent: the mixed-in modules define these ---------------------------
tk_greeting
tk_inner
clamp(1, 2)
1.tk_greeting
nil.tk_greeting
"text".tk_inner
Integer.tk_greeting
tk_under_if
tk_in_def
tk_in_sdef
tk_in_value
tk_in_each
tk_in_map

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
tk_public_sent
tk_var_each
tk_in_lambda
