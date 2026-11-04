# A project-sig-only reopen of a Ruby library class the source requires
# (upstream #2264 / issue #2174, v0.4.6).
#
# When every RBS declaration of a class is the project's own, its root constant
# is a library Ruby ships, and the source requires a feature that loads that
# root, the class's surface is open: `call.undefined-method` declines on both
# sides, and so does `call.raise-non-exception`. The library's own signatures
# are not consulted (`Digest::MakerZzz` declines beside `require "digest/sha2"`).
#
# Must-still-fire controls: a class whose root is no library (`Mine`,
# `Other::K`), and a method the project declaration lists (`own("s")`).
require "matrix"
require "digest/sha2"

Matrix.identity(2)
Matrix.zzz_nope
Matrix.new.zzz
raise Matrix.new if ENV["X"]
Digest::MakerZzz.new.makecc
Mine.new.zzz
Other::K.new.zzz
Matrix.new.own("s")
