//! Compile-tests for the Rust code examples embedded in the repository's Markdown docs.
//!
//! Every ` ```rust ` fenced block in the included files is compiled (and run) as a doctest by
//! `cargo test`, so a real example that stops compiling fails the `test` CI gate. This module is
//! only present while rustdoc collects doctests — its declaration in `lib.rs` is gated behind
//! `#[cfg(doctest)]` — so it never affects normal builds, `cargo doc` output, or clippy.
//!
//! Because doctests are *run* as well as compiled, an example that should type-check but must not
//! execute (it would touch the network, filesystem, or other side effects) should be fenced
//! `rust,no_run`. Reserve `rust,ignore` (or a non-Rust language such as `text` / `bash`) for
//! illustrative snippets that are not meant to compile at all, so they are skipped here entirely.

#[doc = include_str!("../readme.md")]
mod readme {}

#[doc = include_str!("../.github/copilot-instructions.md")]
mod copilot_instructions {}
