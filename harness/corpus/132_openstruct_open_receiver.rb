# OpenStruct is an open receiver (upstream #1456/#1982, v0.4.4).
#
# `OpenStruct` defines each field as a per-instance singleton method and answers
# the rest through `method_missing`, so no signature enumerates its surface. The
# reference names it in `CORE_OVERLAY_OPEN_RECEIVERS` (`unbounded_receiver_surface?`)
# by the BARE class name, on both sides: `call.undefined-method` and
# `call.raise-non-exception` decline, and an inherited Object/Kernel signature no
# longer decides `call.argument-type-mismatch`. At v0.4.3 every row below fired.
#
# Must-still-fire controls: a subclass is not named (`Cfg.new.known.upcase`), and
# an unrelated receiver after the OpenStruct rows still fires.

o = OpenStruct.new(name: "x")
o.name
o.age = 3
o.to_h
o[:name]
o.dig(:name)
o.each_pair
o.no_such_method_zzz
OpenStruct.no_such_method_zzz
o.is_a?("x")
::OpenStruct.new.zz

def raise_open(v)
  raise OpenStruct.new if v
  raise OpenStruct
end

class Cfg < OpenStruct
  def known = 1
end
Cfg.new.known.upcase
1.zzz
