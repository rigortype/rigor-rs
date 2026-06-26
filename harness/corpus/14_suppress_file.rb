# Expected reference diagnostics: (none)
#
# rigor-rs status: SUPPORTED — file-level suppression (# rigor:disable-file)
# The file-level disable silences call.undefined-method everywhere, so the
# L8 typo produces no diagnostic.
# rigor:disable-file undefined-method
s = "x"
s.lenght
