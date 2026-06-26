# Fixture A (ADR-25): config-gated activesupport-core-ext plugin enabled via the
# sidecar 17_plugin_activesupport_core_ext.rigor.yml. With the plugin loaded the
# direct core-ext calls are KNOWN (suppressed on both sides), and the chained
# typo on a String-returning selector witnesses a NEW diagnostic identical to the
# reference: `squish -> String`, so `.foo` is undefined for String.
"x".squish
30.minutes
"x".squish.foo
