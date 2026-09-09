//! Known-track alignment over streaming PCEN spectral features.
//!
//! This first portable core implements the Swift V2 matcher and lock state
//! machine. Capture, PCM resampling and PCEN extraction are caller responsibilities.
//! Feed reference and query features from the same extractor configuration.
//! Call the engine on a worker thread: matching performs allocations and searches.

mod engine;
pub mod ffi;
mod matcher;
pub mod types;

pub use engine::Engine;
pub use types::*;
