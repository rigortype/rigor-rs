# Upstream v0.4.1 (ADR-119; rigor#1578, #1594, #1606, #1623, #1631): a
# definer read off an ancestry Ruby may not order declines.
#
# `def.override-visibility-reduced` reads its parent through
# `DefinerResolution` over a Ruby-order `ResolutionChain`, and a call on a
# project instance types its return the same way. Both decline (the lint stays
# silent, the call types `Dynamic`) where the chain does not stand for the
# name: an unpositioned mixin edge (a conditional, block, hook, receiver-form,
# `send`/`self.` or computed `include`, a declaration reopened under a
# modifier), an external ahead of the definer RBS does not know, a definer or
# visibility that may not hold (a conditional `def`, a `module_function` /
# `private` toggle under a condition, a block or a `begin`). A prepended module
# sits AHEAD of the class, so it is never the parent of the class's own def.
# Every row is oracle-measured at the `v0.4.1` pin; the MUST-STILL-FIRE rows
# sit next to the declines they bound.

# --- def.override-visibility-reduced ---------------------------------------

# (1) MUST STILL FIRE: a plain reduction.
class OvBase1
  def m1 = 1
end
class OvSub1 < OvBase1
  private
  def m1 = 2
end

# (2) a prepended module is ahead of the class: not its parent (p11).
module OvPre2
  def m2 = 1
end
class OvSub2
  prepend OvPre2
  private
  def m2 = 2
end

# (3) `module_function` under a condition / a block / a `begin` (p22, v05, v06).
module OvMf3a
  module_function if ENV["X"]
  def m3a = 1
end
class OvSub3a
  include OvMf3a
  private
  def m3a = 2
end
module OvMf3b
  [1].each { module_function }
  def m3b = 1
end
class OvSub3b
  include OvMf3b
  private
  def m3b = 2
end
module OvMf3c
  begin
    module_function
  end
  def m3c = 1
end
class OvSub3c
  include OvMf3c
  private
  def m3c = 2
end

# (4) MUST STILL FIRE: a certain `public` settles the default again, and a
# direct `module_function` holds the visibility it had (ctl_mf_public, m1).
module OvMf4a
  module_function if ENV["X"]
  public
  def m4a = 1
end
class OvSub4a
  include OvMf4a
  private
  def m4a = 2
end
module OvMf4b
  module_function
  def m4b = 1
end
class OvSub4b
  include OvMf4b
  private
  def m4b = 2
end

# (5) the parent's def may be absent (v01).
class OvBase5
  def m5 = 1 if ENV["X"]
end
class OvSub5 < OvBase5
  private
  def m5 = 2
end

# (6) a named `private :x` that may not run contests the slot.
class OvBase6
  def m6 = 1
  [1].each { private :m6 }
end
class OvSub6 < OvBase6
  private
  def m6 = 2
end

# (7) an uncertain edge on the overriding class: conditional prepend (v12), a
# gem module RBS does not know ahead of the parent (x01), `send(:include)`
# (x05), a hook include (x04), a receiver-form include (x06).
module OvPre7
  def m7 = 1
end
class OvBase7
  def m7 = 0
end
class OvSub7a < OvBase7
  prepend OvPre7 if ENV["X"]
  private
  def m7 = 2
end
class OvSub7b < OvBase7
  include UnknownGemModule7
  private
  def m7 = 2
end
module OvMod7
  private
  def m7 = 0
end
class OvSub7c < OvBase7
  send(:include, OvMod7)
  private
  def m7 = 2
end
module OvHook7
  def self.included(base) = base.include(OvMod7)
end
class OvSub7d < OvBase7
  include OvHook7
  private
  def m7 = 2
end
class OvSub7e < OvBase7
  private
  def m7 = 2
end
OvSub7e.include(OvMod7)

# (8) MUST STILL FIRE: a module RBS knows and that lacks the name, ahead of
# the parent, is passed over.
class OvSub8 < OvBase7
  include Comparable
  private
  def m7 = 2
end

# --- typing: a call through an ancestry the file cannot order is Dynamic ---

# (9) the class's own def, with an unpositioned edge to a module that defines
# the name, to a module that is no project entry, or that the port cannot
# name (u01, u02, u04, w01, w02, w08, x02): silent.
module TyMod9
  def t9a = "m"
  def t9c = "m"
  def t9d = "m"
  def t9e = "m"
  def t9f = "m"
  def t10a = "m"
  def t10b = "m"
  def t10c = "m"
end
class Ty9a
  include TyMod9 if ENV["X"]
  def t9a = 1
end
Ty9a.new.t9a.upcase
module TyEmpty9
end
class Ty9b
  include TyEmpty9 if ENV["X"]
  def t9b = 1
end
Ty9b.new.t9b.upcase
class Ty9c
  [1].each { include TyMod9 }
  def t9c = 1
end
Ty9c.new.t9c.upcase
class Ty9d
  send(:include, TyMod9)
  def t9d = 1
end
Ty9d.new.t9d.upcase
class Ty9e
  include(ENV["X"] ? TyMod9 : Comparable)
  def t9e = 1
end
Ty9e.new.t9e.upcase
class Ty9f
  self.include(TyMod9)
  def t9f = 1
end
Ty9f.new.t9f.upcase
class Ty9g
  include TyEmpty9 if ENV["X"]
  def t9g = nil
end
Ty9g.new.t9g.upcase

# (10) a receiver-form include, a modifier reopen, a hook include (u06, w04,
# w06): silent.
class Ty10a
  def t10a = 1
end
Ty10a.include(TyMod9)
Ty10a.new.t10a.upcase
class Ty10b
  def t10b = 1
end
class Ty10b
  include TyMod9
end if ENV["X"]
Ty10b.new.t10b.upcase
module TyHook10
  def self.included(base)
    base.include(TyMod9)
  end
end
class Ty10c
  include TyHook10
  def t10c = 1
end
Ty10c.new.t10c.upcase

# (11) MUST STILL FIRE: an unconditional include, a conditional include of a
# module RBS knows to lack the name, `singleton_class.include`, an
# `obj.extend` and a `class_eval` on a local (w12, w03, w07, w09), and a
# conditional include of a project module whose closure cannot answer the
# name (`Relevance` discharges the mark).
class Ty11a
  include TyMod9
  def t11a = 1
end
Ty11a.new.t11a.upcase
class Ty11b
  include Comparable if ENV["X"]
  def t11b = 1
end
Ty11b.new.t11b.upcase
class Ty11c
  singleton_class.include(TyMod9)
  def t11c = 1
end
Ty11c.new.t11c.upcase
class Ty11d
  def t11d = 1
end
o11 = Object.new
o11.extend(TyMod9)
Ty11d.new.t11d.upcase
class Ty11e
  def t11e = 1
end
k11 = Ty11e
k11.class_eval { include TyMod9 }
Ty11e.new.t11e.upcase
module TyLacks11
  def other11 = "m"
end
class Ty11f
  include TyLacks11 if ENV["X"]
  def t11f = 1
end
Ty11f.new.t11f.upcase
