//! Compile-tests for the Rust code examples embedded in the repository's Markdown docs.
//!
//! Every ` ```rust ` fenced block in the included files is compiled (and run) as a doctest by
//! `cargo test`, so a real example that stops compiling fails the `test` CI gate. This module is
//! only present while rustdoc collects doctests — its declaration in `lib.rs` is gated behind
//! `#[cfg(doctest)]` — so it never affects normal builds, `cargo doc` output, or clippy.
//!
//! Illustrative snippets that are not meant to compile must be fenced `rust,ignore` (or with a
//! non-Rust language such as `text` / `bash`) in the source docs so they are skipped here.

#[doc = include_str!("../readme.md")]
mod readme {}

#[doc = include_str!("../.github/copilot-instructions.md")]
mod copilot_instructions {}
