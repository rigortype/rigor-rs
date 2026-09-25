# `X.class_eval do def m end` attributes `m` to `X`, NOT to the toplevel def
# table — upstream `fb781023` (rigortype/rigor#1135, this repo's #141). A bare
# `m` afterwards fires `call.unresolved-toplevel`; the pre-change port kept the
# def in `<toplevel>` and stayed silent (the `expect`/`to` sweep family).
#
# The special cases, all oracle-measured at the `e59b7b89` pin, one fresh temp
# cwd per case, `--no-cache`:
#
#   * `Object`'s instance methods ARE the toplevel methods — `Object.class_eval
#     { def m }` (and `::Object`, and the `attr_*` / `define_method` /
#     `alias_method` macros inside one) keeps `m` bare-callable.
#     `Kernel`/`BasicObject`/`String` do NOT collapse.
#   * `instance_eval`/`instance_exec` bind their `def`s on the receiver's
#     singleton, so `Object.instance_eval { def m }` does NOT reach toplevel
#     either — only `class_eval`/`module_eval`-style instance-side defs do.
#   * A `class <<` body or a self/owner-targeting `def Object.x` inside an eval
#     stays singleton-side (`def_singleton?` → `record_def_node` skips it).
#   * A dynamic, unresolved or local receiver names no owner — its defs file
#     nowhere. A BARE `class_eval` keeps the enclosing self — which toplevel
#     cannot name — so its defs file nowhere too (`helper_bare` probe).
#   * An anonymous `Class.new`/`Module.new` body keeps the #319 toplevel
#     leniency; a NAMED `K = Class.new` write targets `K` and does not.

class Target; end

# --- defs that must NOT reach toplevel: every bare call below FIRES ----------

Target.class_eval do
  def eval_def = 1
end
Target.module_eval do
  def module_eval_def = 2
end
Target.instance_eval do
  def singleton_side_def = 3
end
Target.instance_exec do
  def exec_side_def = 4
end
Kernel.class_eval do
  def kernel_eval_def = 5
end
BasicObject.class_eval do
  def basic_eval_def = 6
end
Object.instance_eval do
  def object_instance_def = 7
end
String.class_eval do
  def self.string_singleton_def = 8
end
String.class_eval do
  class << self
    def inside_singleton_class_def = 9
  end
end
Object.class_eval do
  def Object.object_receiver_def = 10
end
Object.class_eval do
  class << self
    def object_singleton_body_def = 11
  end
end
Target.class_eval do
  attr_reader :target_eval_attr
  define_method(:target_eval_dm) { 12 }
  alias_method :target_eval_alias, :eval_def
end
tgt = Target
tgt.class_eval do
  def dynamic_receiver_def = 13
end
Undeclared.class_eval do
  def unresolved_receiver_def = 14
end
NamedK = Class.new do
  def named_factory_def = 15
end
class_eval do
  def bare_eval_def = 16
end
self.class_eval do
  def self_eval_def = 17
end

eval_def
module_eval_def
singleton_side_def
exec_side_def
kernel_eval_def
basic_eval_def
object_instance_def
string_singleton_def
inside_singleton_class_def
object_receiver_def
object_singleton_body_def
target_eval_attr
target_eval_dm
target_eval_alias
dynamic_receiver_def
unresolved_receiver_def
named_factory_def
bare_eval_def
self_eval_def

# --- defs that DO reach toplevel: every bare call below STAYS SILENT ---------

Object.class_eval do
  def object_eval_def = 18
end
::Object.class_eval do
  def rooted_object_def = 19
end
Object.class_eval do
  attr_reader :object_eval_attr
  attr_accessor :object_eval_acc
  define_method(:object_eval_dm) { 20 }
  alias_method :object_eval_alias, :object_eval_def
end
Module.new do
  def anon_module_def = 21
end
Object.class_eval do
  class_eval do
    def nested_object_def = 22
  end
end

object_eval_def
rooted_object_def
object_eval_attr
object_eval_acc
object_eval_dm
object_eval_alias
anon_module_def
nested_object_def

# --- must-still-fire controls ------------------------------------------------

# A bare toplevel `def` still registers.
def control_toplevel_def = 23
# And a genuinely undefined name fires.
undefined_anywhere

control_toplevel_def
