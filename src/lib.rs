//! Source analysis for Cranpose's parameter comparison and skipping rules.
//! The analyzer never expands macros, runs build scripts, or changes application code.
mod analyze;
mod index;
pub mod model;
pub mod output;
pub mod project;
mod types;
pub use analyze::analyze;
pub use model::*;
