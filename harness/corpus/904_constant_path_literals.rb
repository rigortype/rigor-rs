# rigor-rs#904: path writes keep the literal value for qualified and bare reads.
Float::NAN = 3
Float::NAN.bogus
module Float
  NAN.bogus
end
::ARGV = 5
ARGV.bogus
::ARGV.bogus
Math::PI = 5
Math::PI.bogus
M904::ARR = [1]
M904::ARR.bogus
M904::STR = "x"
M904::STR.bogus
M904::HASH = { a: 1 }
M904::HASH.bogus
M904::EMPTY_ARR = []
M904::EMPTY_ARR.bogus
M904::EMPTY_HASH = {}
M904::EMPTY_HASH.bogus
M904::EMPTY_STR = ""
M904::EMPTY_STR.bogus
M904::FROZEN = [1].freeze
M904::FROZEN.bogus
M904::NILV = nil
M904::NILV.bogus
module N904
  module M904
  end
  M904::K = 5
  M904::K.bogus
end
