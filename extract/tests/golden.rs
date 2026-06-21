//! Golden-sample regression tests.
//!
//! Each platform has a small committed fixture of real downloaded data under
//! `tests/fixtures/<platform>-data.jsonl`, chosen to cover the variants that have
//! bitten us before. For each platform we:
//!
//! 1. Load the fixture through the same code path as the real extractor
//!    (`Platform::load_data`). This is the deserialization lock: if a struct stops
//!    matching the saved schema, loading fails here.
//! 2. Standardize every item and snapshot a deterministic summary of the result
//!    (via `insta`). This is the logic lock: if standardization output drifts
//!    (e.g. a unit conversion regresses), the snapshot mismatches.
//! 3. Assert a few invariants on every standardized market.
//!
//! These fixtures are frozen, so they catch *our* regressions, not live API drift.
//! Regenerate them with the live APIs when an intentional change lands; see the
//! scripts referenced in the roadmap's schema-drift item.

use std::path::PathBuf;

use themis_extract::MarketError;
use themis_extract::platforms::{Platform, PlatformExt};

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// Round to four decimals so snapshots don't churn on float noise.
fn r4(x: f32) -> f32 {
    (x * 10_000.0).round() / 10_000.0
}

/// Pull the market ID and a stable tag out of a standardization error.
fn err_parts(e: &MarketError) -> (String, &'static str) {
    match e {
        MarketError::NotAMarket(id) => (id.clone(), "NotAMarket"),
        MarketError::MarketNotResolved(id) => (id.clone(), "MarketNotResolved"),
        MarketError::MarketCancelled(id) => (id.clone(), "MarketCancelled"),
        MarketError::NoMarketTrades(id) => (id.clone(), "NoMarketTrades"),
        MarketError::InvalidMarketTrades(id, _) => (id.clone(), "InvalidMarketTrades"),
        MarketError::DataInvalid(id, _) => (id.clone(), "DataInvalid"),
        MarketError::ProcessingError(id, _) => (id.clone(), "ProcessingError"),
        MarketError::MarketTypeNotImplemented(id, _) => (id.clone(), "MarketTypeNotImplemented"),
    }
}

/// A deterministic, snapshot-friendly summary of one standardized market or skip.
#[derive(serde::Serialize)]
#[serde(untagged)]
enum Entry {
    Market {
        id: String,
        title: String,
        platform: String,
        category: Option<String>,
        resolution: f32,
        open: String,
        close: String,
        duration_days: u32,
        volume_usd: Option<f32>,
        traders: Option<u32>,
        daily_probs: usize,
        prob_first: f32,
        prob_last: f32,
        prob_min: f32,
        prob_max: f32,
        criteria: usize,
    },
    Skipped {
        id: String,
        skipped: String,
    },
}

impl Entry {
    fn sort_key(&self) -> (String, u8) {
        match self {
            Entry::Market { id, .. } => (id.clone(), 0),
            Entry::Skipped { id, .. } => (id.clone(), 1),
        }
    }
}

/// Load a platform's fixture, standardize every item, assert invariants, and
/// return a sorted, snapshot-friendly summary.
fn summarize(platform: Platform) -> Vec<Entry> {
    // `fail_fast = true` turns any deserialization failure into a hard error.
    let items = platform
        .load_data(&fixtures_dir(), &true)
        .unwrap_or_else(|e| panic!("{platform}: failed to load fixture: {e}"));
    assert!(
        !items.is_empty(),
        "{platform}: fixture loaded zero items — is the fixture file present?"
    );

    let mut entries = Vec::new();
    for item in items {
        match platform.standardize(item) {
            Ok(markets) => {
                for mp in markets {
                    let m = &mp.market;

                    // Invariants that should hold for every standardized market.
                    assert!(
                        m.resolution == 0.0 || m.resolution == 1.0,
                        "{}: resolution {} is not binary",
                        m.id,
                        m.resolution
                    );
                    assert!(
                        m.close_datetime >= m.open_datetime,
                        "{}: close before open",
                        m.id
                    );
                    for dp in &mp.daily_probabilities {
                        assert!(
                            (0.0..=1.0).contains(&dp.prob),
                            "{}: probability {} out of [0, 1]",
                            m.id,
                            dp.prob
                        );
                    }

                    let probs: Vec<f32> =
                        mp.daily_probabilities.iter().map(|d| r4(d.prob)).collect();
                    let (prob_first, prob_last, prob_min, prob_max) = if probs.is_empty() {
                        (0.0, 0.0, 0.0, 0.0)
                    } else {
                        (
                            probs[0],
                            *probs.last().unwrap(),
                            probs.iter().copied().fold(f32::INFINITY, f32::min),
                            probs.iter().copied().fold(f32::NEG_INFINITY, f32::max),
                        )
                    };

                    entries.push(Entry::Market {
                        id: m.id.clone(),
                        title: m.title.clone(),
                        platform: m.platform_slug.clone(),
                        category: m.category_slug.clone(),
                        resolution: m.resolution,
                        open: m.open_datetime.to_rfc3339(),
                        close: m.close_datetime.to_rfc3339(),
                        duration_days: m.duration_days,
                        volume_usd: m.volume_usd.map(r4),
                        traders: m.traders_count,
                        daily_probs: mp.daily_probabilities.len(),
                        prob_first,
                        prob_last,
                        prob_min,
                        prob_max,
                        criteria: mp.criterion_probabilities.len(),
                    });
                }
            }
            Err(e) => {
                let (id, tag) = err_parts(&e);
                entries.push(Entry::Skipped {
                    id,
                    skipped: tag.to_string(),
                });
            }
        }
    }

    entries.sort_by_key(Entry::sort_key);
    entries
}

#[test]
fn kalshi_golden() {
    insta::assert_yaml_snapshot!(summarize(Platform::Kalshi));
}

#[test]
fn manifold_golden() {
    insta::assert_yaml_snapshot!(summarize(Platform::Manifold));
}

#[test]
fn metaculus_golden() {
    insta::assert_yaml_snapshot!(summarize(Platform::Metaculus));
}

#[test]
fn polymarket_golden() {
    insta::assert_yaml_snapshot!(summarize(Platform::Polymarket));
}
