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
