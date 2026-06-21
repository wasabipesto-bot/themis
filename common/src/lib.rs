//! Types and helpers shared across the themis workspace crates.

use clap::ValueEnum;
use serde::Serialize;
use std::fmt;

/// The prediction-market platforms this project supports.
///
/// This is the canonical platform enum used by the downloader and extractor.
/// (The grader's `Platform` is a different thing — a database row describing a
/// platform — and is not related to this type.)
#[derive(Debug, Copy, Clone, PartialEq, Eq, ValueEnum, Serialize)]
pub enum Platform {
    Kalshi,
    Manifold,
    Metaculus,
    Polymarket,
}

impl Platform {
    /// Returns a list of all supported platforms.
    pub fn all() -> Vec<Platform> {
        vec![
            Platform::Kalshi,
            Platform::Manifold,
            Platform::Metaculus,
            Platform::Polymarket,
        ]
    }
}

impl fmt::Display for Platform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Platform::Kalshi => "Kalshi",
            Platform::Manifold => "Manifold",
            Platform::Metaculus => "Metaculus",
            Platform::Polymarket => "Polymarket",
        };
        write!(f, "{name}")
    }
}
