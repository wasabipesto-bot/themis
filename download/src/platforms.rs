//! A couple items related to platforms.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use clap::ValueEnum;
use log::{debug, error, info, warn};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use serde_jsonlines::json_lines;
use std::collections::HashSet;
use std::fmt;
use std::path::Path;

use crate::util::{backup_file, index_file_is_valid, load_data_ids};

pub mod kalshi;
pub mod manifold;
pub mod metaculus;
pub mod polymarket;

/// Format of data saved to JSON for basic index data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexItem {
    pub id: String,
    pub last_updated: DateTime<Utc>,
    pub data: Value,
}

/// All possible platforms that are supported by this application.
#[derive(Debug, Copy, Clone, PartialEq, Eq, ValueEnum, Serialize)]
pub enum Platform {
    Kalshi,
    Manifold,
    Metaculus,
    Polymarket,
}
impl fmt::Display for Platform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Platform::Kalshi => write!(f, "Kalshi"),
            Platform::Manifold => write!(f, "Manifold"),
            Platform::Metaculus => write!(f, "Metaculus"),
            Platform::Polymarket => write!(f, "Polymarket"),
        }
    }
}
impl Platform {
    /// Returns a list of all supported platform types.
    pub fn all() -> Vec<Platform> {
        vec![
            Platform::Kalshi,
            Platform::Manifold,
            Platform::Metaculus,
            Platform::Polymarket,
        ]
    }
    fn get_close_datetime(&self, item: &IndexItem) -> Option<DateTime<Utc>> {
        match self {
            Platform::Kalshi => {
                let date = item.data.get("close_time")?.as_str()?;
                let date = DateTime::parse_from_rfc3339(date)
                    .expect("Failed to parse Kalshi close_time")
                    .with_timezone(&Utc);
                Some(date)
            }
            Platform::Manifold => {
                let date = item.data.get("resolutionTime")?.as_number()?.as_i64()?;
                let date = DateTime::from_timestamp_millis(date)
                    .expect("Failed to parse Manifold resolutionTime")
                    .with_timezone(&Utc);
                Some(date)
            }
            Platform::Metaculus => {
                let date = item.data.get("actual_close_time")?.as_str()?;
                let date = DateTime::parse_from_rfc3339(date)
                    .expect("Failed to parse Metaculus actual_close_time")
                    .with_timezone(&Utc);
                Some(date)
            }
            Platform::Polymarket => {
                let date = item.data.get("end_date_iso")?.as_str()?;
                let date = DateTime::parse_from_rfc3339(date)
                    .expect("Failed to parse Polymarket end_date_iso")
                    .with_timezone(&Utc);
                Some(date)
            }
        }
    }
    /// Streams the index file and returns the set of IDs that still need downloading.
    /// We stream rather than loading the whole index into memory because some platforms
    /// (notably Kalshi) have hundreds of thousands of markets and the full index of raw
    /// JSON values can use tens of gigabytes of memory.
    fn get_ids_to_download(
        &self,
        index_file_path: &Path,
        data_ids: &HashSet<String>,
        resolved_since: &Option<DateTime<Utc>>,
    ) -> Result<HashSet<String>> {
        let now = Utc::now();
        let mut ids_to_download = HashSet::new();
        let mut total = 0usize;

        for item in json_lines::<IndexItem, _>(index_file_path)
            .with_context(|| format!("Failed to open index file {}", index_file_path.display()))?
        {
            let item = item.context("Failed to read item from index file")?;
            total += 1;

            // Skip if already downloaded
            if data_ids.contains(&item.id) {
                continue;
            }

            if let Some(cutoff_date) = resolved_since {
                match self.get_close_datetime(&item) {
                    None => {
                        // Skip if market is not resolved yet
                        // Or if resolution date is just missing
                        continue;
                    }
                    Some(resolved_at) => {
                        // Skip if resolution date is before cutoff
                        if &resolved_at < cutoff_date {
                            continue;
                        }
                        // Skip if resolution date is in the future
                        if now < resolved_at {
                            continue;
                        }
                    }
                }
            }

            // Add item to the download list
            ids_to_download.insert(item.id);
        }

        debug!(
            "{self}: Selected {}/{} items to download",
            ids_to_download.len(),
            total,
        );
        Ok(ids_to_download)
    }
}
pub trait PlatformHandler {
    fn download(
        &self,
        output_dir: &Path,
        reset_index: &bool,
        reset_cache: &bool,
        resolved_since: &Option<DateTime<Utc>>,
    ) -> impl std::future::Future<Output = ()> + Send;
}
impl PlatformHandler for Platform {
    async fn download(
        &self,
        output_dir: &Path,
        reset_index: &bool,
        reset_cache: &bool,
        resolved_since: &Option<DateTime<Utc>>,
    ) {
        // build file paths
        let index_file_path = output_dir.join(format!("{self}-index.jsonl").to_lowercase());
        let data_file_path = output_dir.join(format!("{self}-data.jsonl").to_lowercase());

        // back up files if requested
        if *reset_index || *reset_cache {
            info!("{self}: Backing up file {}", index_file_path.display());
            backup_file(&index_file_path).unwrap_or_else(|e| {
                error!("Failed to back up file {}: {e}", index_file_path.display());
                panic!();
            });
        }
        if *reset_cache {
            info!("{self}: Backing up file {}", data_file_path.display());
            backup_file(&data_file_path).unwrap_or_else(|e| {
                error!("Failed to back up file {}: {e}", data_file_path.display());
                panic!();
            });
        }

        // Download the index if we don't already have a valid one on disk.
        // The index is streamed to disk and read back lazily so we never hold the
        // whole thing in memory (Kalshi alone is hundreds of thousands of markets).
        if index_file_is_valid(&index_file_path) {
            info!("{self}: Using existing index on disk.");
        } else {
            info!("{self}: Downloading new index.");
            let result = match self {
                Platform::Kalshi => kalshi::download_index(&index_file_path).await,
                Platform::Manifold => manifold::download_index(&index_file_path).await,
                Platform::Metaculus => metaculus::download_index(&index_file_path).await,
                Platform::Polymarket => polymarket::download_index(&index_file_path).await,
            };
            if let Err(e) = result {
                error!("{self}: Failed to download index: {e}");
                panic!();
            }
            info!("{self}: Index downloaded and saved to disk.");
        }

        // load the data file from the disk
        // if it does not exist, create an empty file
        // note that this can be very large
        info!("{self}: Loading cached data progress from disk.");
        let data_ids = load_data_ids(&data_file_path).unwrap();
        info!(
            "{self}: Data cache loaded from disk with {} items.",
            data_ids.len()
        );

        // stream the index file to find the IDs that aren't in the data file
        debug!("{self}: Getting IDs to download.");
        let ids_to_download = self
            .get_ids_to_download(&index_file_path, &data_ids, resolved_since)
            .unwrap_or_else(|e| {
                error!("{self}: Failed to read index file: {e}");
                panic!();
            });
        let num_to_download = ids_to_download.len();

        // check if anything needs to be downloaded
        if num_to_download == 0 {
            info!("{self}: All {} items already downloaded.", data_ids.len());
        } else {
            info!(
                "{self}: Starting data download: {} downloaded, {} pending",
                data_ids.len(),
                num_to_download
            );
            if let Err(err) = match self {
                Platform::Kalshi => {
                    kalshi::download_data(&index_file_path, &ids_to_download, &data_file_path).await
                }
                Platform::Manifold => {
                    manifold::download_data(&index_file_path, &ids_to_download, &data_file_path)
                        .await
                }
                Platform::Metaculus => {
                    metaculus::download_data(&index_file_path, &ids_to_download, &data_file_path)
                        .await
                }
                Platform::Polymarket => {
                    polymarket::download_data(&index_file_path, &ids_to_download, &data_file_path)
                        .await
                }
            } {
                error!("{self}: Error downloading data: {}", err);
                panic!();
            }
            debug!("{self}: Main download task complete.");

            // confirm how many we actually got
            debug!("{self}: Checking data on disk.");
            let downloaded_ids = load_data_ids(&data_file_path).unwrap();
            let num_downloaded = ids_to_download
                .iter()
                .filter(|id| downloaded_ids.contains(*id))
                .count();
            if num_downloaded == num_to_download {
                info!("{self}: All {} items downloaded", num_to_download);
            } else {
                let percentage = if num_to_download > 0 {
                    (num_downloaded as f64 / num_to_download as f64) * 100.0
                } else {
                    0.0
                };
                warn!(
                    "{self}: {} out of {} items downloaded ({:.1}%)",
                    num_downloaded, num_to_download, percentage
                );
                warn!("Re-run the download program to retry the failed items.");
            }
        }
    }
}
