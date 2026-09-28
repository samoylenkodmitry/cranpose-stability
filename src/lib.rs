//! Source analysis for Cranpose's parameter comparison and skipping rules.
//! The analyzer never runs Cargo, build scripts or procedural macros, and never changes
//! application code; dependency sources are read from Cargo's local cache when present.
mod analyze;
mod deps;
mod index;
mod macros;
pub mod model;
pub mod output;
mod parse;
pub mod project;
mod types;
pub use analyze::analyze;
pub use model::*;
