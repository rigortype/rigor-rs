# def.override-visibility-reduced — positive across an INCLUDED MODULE ancestor.
# `Greeter#hello` is public; the including class narrows it to private. The MRO
# walk reaches the included module first, so the rule fires (overrides
# Greeter#hello).
module Greeter
  def hello; end
end

class Robot
  include Greeter

  private

  def hello; end
end
