//! Parsing (ADR-0003) + lowering to an owned, `NodeId`-indexed AST (ADR-0012).
//!
//! Verified in the spike: the official `ruby-prism` binding builds offline
//! (libprism via clang) and exposes comments + precise node spans + error
//! recovery. Lowering the borrowed Prism tree into owned nodes (to free
//! inference from the parse-buffer lifetime) is TODO.
#![allow(dead_code)]

pub use ruby_prism;

/// Parse Ruby source with Prism. The borrowed result will be lowered into an
/// owned, `NodeId`-indexed AST before inference (ADR-0012).
pub fn parse(source: &[u8]) -> ruby_prism::ParseResult<'_> {
    ruby_prism::parse(source)
}
