//! Library facade for `pidory`.
//!
//! Existed pre-#286 to expose `subprocess::parser` for integration tests
//! under `tests/`. #286 added `pub mod claude_settings;` so that doctests in
//! `claude_settings` can compile against `pidory::claude_settings::*`.
//!
//! The binary entry point lives in `src/main.rs` and currently re-declares
//! its own `mod claude_settings;` (binary + library dual compilation). A
//! follow-up cleanup (P1.4+) can switch the binary to `use pidory::...` to
//! eliminate the duplication.

pub mod claude_settings;

pub mod subprocess {
    pub mod parser;
}
