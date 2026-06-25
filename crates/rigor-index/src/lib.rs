//! The index layer (ADR-0004): declaration discovery, ancestor linearization
//! (with visibility), constant/method resolution, built on the `ruby-rbs`
//! parser behind a rigor-rs-owned trait. Rubydex is an optional accelerator.
//!
//! Verified in the spike: RBS exposes typed method definitions (return types,
//! parameter types, variance, overloads, generics) — see spike/probe_rbs.rb.
//! The Rust `ruby-rbs` crate parses the same grammar (network-gated to confirm
//! its public API surfaces them; else a thin extraction layer over its AST).
#![allow(dead_code)]
