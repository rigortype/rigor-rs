# def.override-visibility-reduced — a reopened-class split. The parent `Base`
# defines `compute` publicly in one body; `Derived` (a separate body) narrows it
# to private. Both tools resolve the parent across the reopened/split views and
# fire once on Derived#compute.
class Base
  def compute; end
end

class Base
  def other; end
end

class Derived < Base
  private

  def compute; end
end
