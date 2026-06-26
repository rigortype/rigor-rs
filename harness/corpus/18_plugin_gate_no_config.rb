# Fixture B (ADR-25 gate guard): the SAME ActiveSupport core-ext selectors but
# with NO sidecar `.rigor.yml`. Without the plugin both rigor-rs and the
# plugin-less reference must STILL flag every direct call — proving the plugin is
# applied ONLY when config-gated, never unconditionally.
"x".squish
30.minutes
"x".squish.foo
