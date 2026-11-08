# rigor-rs#494: `M#nope` overloads no original, so `M`'s instance build fails
# (`InvalidOverloadMethodError`); `Inc` includes `M`, so ITS instance build
# fails through the ancestor closure — and the reference builds the singleton
# definition on the instance one, so `Inc.nosuch` is silent too. Neither
# `Inc`'s methods nor the absent `nosuch` may witness.
Inc.nosuch
Inc.iok
