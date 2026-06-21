//! Tools to download and process markets from the Kalshi API.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use log::{debug, error, trace, warn};
use reqwest_middleware::ClientWithMiddleware;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use serde_jsonlines::{append_json_lines, json_lines};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{LazyLock, Mutex};
use std::time::Instant;

use super::{IndexItem, Platform};
use crate::util::{
    display_progress, finalize_temp_file, get_reqwest_client_ratelimited, get_temp_file_path,
    send_request,
};

const KALSHI_API_BASE: &str = "https://api.elections.kalshi.com/trade-api/v2";
const KALSHI_RATELIMIT: usize = 10;
const KALSHI_RATELIMIT_MS: u64 = 1000;

// Caches for event and series data to avoid repeated lookups.
static EVENT_CACHE: LazyLock<Mutex<HashMap<String, Value>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static SERIES_CACHE: LazyLock<Mutex<HashMap<String, Value>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Format of data saved to JSON
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KalshiItem {
    id: String,
    last_updated: DateTime<Utc>,
    market: Value,
    event: Value,
    series: Value,
    history: Vec<Value>,
}

/// Downloads the event data for this market.
/// An event usually contains multiple markets and has more information.
async fn get_event(client: &ClientWithMiddleware, market: &Value) -> Result<Value> {
    let event_ticker = market
        .get("event_ticker")
        .context("Expected 'event_ticker' field in market.")?
        .as_str()
        .context("Failed to interpret 'event_ticker' as string.")?;

    // Check cache first
    if let Some(cached_event) = EVENT_CACHE.lock().unwrap().get(event_ticker) {
        trace!("Cache hit for event ticker: {event_ticker}");
        return Ok(cached_event.clone());
    }

    // If not in cache, download the event
    let api_url = format!("{KALSHI_API_BASE}/events/{event_ticker}");
    let response = send_request(client.get(&api_url)).await?;
    let event = response
        .get("event")
        .context("Expected 'event' field in /events response.")?
        .clone();

    // Add to cache
    EVENT_CACHE
        .lock()
        .unwrap()
        .insert(event_ticker.to_owned(), event.clone());
    Ok(event)
}

/// Downloads the series data for this market.
/// A series usually contains multiple events and has more information.
/// You can usually guess the series ticker from the market ticker but there are a lot of gotchas.
/// Instead, we get the series ticker from the event data, which we get from the event ticker from the market.
async fn get_series(client: &ClientWithMiddleware, event: &Value) -> Result<Value> {
    let series_ticker = event
        .get("series_ticker")
        .context("Expected 'series_ticker' field in event.")?
        .as_str()
        .context("Failed to interpret 'series_ticker' as string.")?;

    // Check cache first
    if let Some(cached_series) = SERIES_CACHE.lock().unwrap().get(series_ticker) {
        trace!("Cache hit for series ticker: {series_ticker}");
        return Ok(cached_series.clone());
    }

    // If not in cache, download the series
    let api_url = format!("{KALSHI_API_BASE}/series/{series_ticker}");
    let response = send_request(client.get(&api_url)).await?;
    let series = response
        .get("series")
        .context("Expected 'series' field in /series response.")
        .cloned()?;

    // Add to cache
    SERIES_CACHE
        .lock()
        .unwrap()
        .insert(series_ticker.to_owned(), series.clone());
    Ok(series)
}

/// Download extended data from the `/markets/trades` endpoint.
/// Detect errors and warn but don't stop processing.
async fn get_trades(
    client: &ClientWithMiddleware,
    market: &Value,
    ticker: &str,
) -> Result<Vec<Value>> {
    // Kalshi has a *lot* of markets and the primary bottleneck to this
    // download is getting trade history. Most don't have any trade volume
    // so to avoid sending pointless requests that will return 0 trades,
    // we skip the request if the volume is 0.
    // Kalshi migrated to fixed-point fields, so volume is now `volume_fp`,
    // a stringified decimal (e.g. "9545.44") rather than an integer.
    let volume = market
        .get("volume_fp")
        .context("Expected 'volume_fp' field in market.")?
        .as_str()
        .context("Failed to interpret 'volume_fp' as string.")?
        .parse::<f64>()
        .context("Failed to parse 'volume_fp' as a number.")?;
    if volume == 0.0 {
        trace!("Kalshi volume is 0, skipping /trades request.");
        return Ok(Vec::new());
    }

    // prep for requests
    let api_url = KALSHI_API_BASE.to_owned() + "/markets/trades";
    let limit: usize = 1000;

    // loop until we have all history items
    let mut cursor: Option<String> = None;
    let mut all_trades = Vec::new();
    loop {
        let response = send_request(
            client
                .get(&api_url)
                .query(&[("limit", limit)])
                .query(&[("ticker", ticker)])
                .query(&[("cursor", cursor.clone())])
                .query(&[("min_ts", 0)]),
        )
        .await?;

        // get history array and save
        let trades = response
            .get("trades")
            .context("Expected 'trades' field in response.")?
            .as_array()
            .context("Failed to interpret 'trades' as array.")?
            .to_owned();
        all_trades.extend(trades.clone());

        // warn if there seems like too many trades
        // I had an issue once where it just kept going until it OOM'd
        if all_trades.len() > 500_000 && all_trades.len() % (limit * 10) == 0 {
            warn!(
                "Kalshi market {ticker} has accumulated {} trades, something may be wrong. Curent cursor: {}. Last trade: {:?}",
                all_trades.len(),
                cursor.unwrap(),
                all_trades.last().unwrap()
            );
        }

        // update the cursor or break
        if trades.len() == limit {
            let cursor_some = response
                .get("cursor")
                .context("Expected 'cursor' field in response.")?
                .as_str()
                .context("Failed to interpret 'cursor' as string.")?
                .to_owned();
            trace!(
                "Got {} items and new Kalshi history cursor: {cursor_some}",
                trades.len()
            );
            if cursor_some.is_empty() {
                debug!("Market returned {limit} trades but cursor was empty. Exiting.");
                break;
            }
            cursor = Some(cursor_some);
        } else {
            trace!(
                "Batch size {} was smaller than limit {}, we must be done here.",
                trades.len(),
                limit
            );
            break;
        }
    }

    Ok(all_trades)
}

/// Downloads everything to build a market item.
async fn get_data_and_build_item(
    client: &ClientWithMiddleware,
    item: &IndexItem,
) -> Result<KalshiItem> {
    // get market from the streamed index item
    let market = item.data.clone();
    let ticker = item.id.as_str();
    // get event data...
    let event = get_event(client, &market).await?;
    // and series data...
    let series = get_series(client, &event).await?;
    // return the row ready for writing
    Ok(KalshiItem {
        id: item.id.clone(),
        last_updated: Utc::now(),
        market: market.clone(),
        event,
        series,
        history: get_trades(client, &market, ticker).await?,
    })
}

/// Downloads a new index, streaming it directly to disk.
pub async fn download_index(index_file_path: &Path) -> Result<()> {
    // set platform
    let platform = Platform::Kalshi;

    // get url, client, login token
    let api_url = KALSHI_API_BASE.to_owned() + "/markets";
    let client = get_reqwest_client_ratelimited(KALSHI_RATELIMIT, KALSHI_RATELIMIT_MS);

    // write batches to a temp file first, then atomically move it into place
    let temp_file_path = get_temp_file_path(index_file_path);
    let _ = std::fs::remove_file(&temp_file_path);

    // loop through questions endpoint until all are downloaded
    let limit = 1000;
    let mut total = 0usize;
    let mut cursor: Option<String> = None;
    loop {
        let response = send_request(
            client
                .get(&api_url)
                .query(&[("limit", limit)])
                .query(&[("cursor", cursor.clone())]),
        )
        .await?;
        let batch = response
            .get("markets")
            .context("Expected 'markets' field in response.")?
            .as_array()
            .context("Failed to interpret 'markets' as array.")?
            .to_owned();

        // build items from batch and stream them straight to the temp file
        let mut items = Vec::with_capacity(batch.len());
        for market in batch.clone() {
            let market_ticker = market
                .get("ticker")
                .context("Expected 'ticker' field in response.")?
                .as_str()
                .context("Failed to interpret 'ticker' as string.")?
                .to_owned();
            items.push(IndexItem {
                id: market_ticker,
                last_updated: Utc::now(),
                data: market,
            });
        }
        append_json_lines(&temp_file_path, items)?;
        total += batch.len();

        // update the cursor or break
        if batch.len() == limit {
            let cursor_some = response
                .get("cursor")
                .context("Expected 'cursor' field in response.")?
                .as_str()
                .context("Failed to interpret 'cursor' as string.")?
                .to_owned();
            debug!(
                "Got {} items and new {platform} cursor: {cursor_some}",
                batch.len()
            );
            cursor = Some(cursor_some);
        } else {
            debug!(
                "Batch size {} was smaller than limit {}, we must be done here.",
                batch.len(),
                limit
            );
            break;
        }
    }

    // atomically move the completed temp file into place
    finalize_temp_file(&temp_file_path, index_file_path)?;
    debug!("{platform}: Index download complete with {total} total items");
    Ok(())
}

/// Downloads extended data for all markets that haven't been downloaded.
/// Appends directly into data file.
pub async fn download_data(
    index_file_path: &Path,
    ids_to_download: &HashSet<String>,
    data_file_path: &Path,
) -> Result<()> {
    // Get client
    let platform = Platform::Kalshi;
    let client = get_reqwest_client_ratelimited(KALSHI_RATELIMIT, KALSHI_RATELIMIT_MS);

    // Set progress counters
    let start_time = Instant::now();
    let download_count = ids_to_download.len();
    let mut completed: usize = 0;

    // Stream the index file and process matching items in concurrent batches of 10,
    // so we never hold the whole index in memory.
    let mut batch: Vec<IndexItem> = Vec::with_capacity(10);
    for item in json_lines::<IndexItem, _>(index_file_path)
        .with_context(|| format!("Failed to open index file {}", index_file_path.display()))?
    {
        let item = item.context("Failed to read item from index file")?;
        if !ids_to_download.contains(&item.id) {
            continue;
        }
        batch.push(item);
        if batch.len() >= 10 {
            completed += flush_batch(&client, &mut batch, data_file_path).await?;
            display_progress(&platform, completed, download_count, &start_time);
        }
    }
    if !batch.is_empty() {
        completed += flush_batch(&client, &mut batch, data_file_path).await?;
        display_progress(&platform, completed, download_count, &start_time);
    }
    Ok(())
}

/// Downloads a batch of items concurrently, appends the successful ones to disk,
/// logs any errors, clears the batch, and returns the number processed.
async fn flush_batch(
    client: &ClientWithMiddleware,
    batch: &mut Vec<IndexItem>,
    data_file_path: &Path,
) -> Result<usize> {
    let count = batch.len();
    let futures = batch
        .iter()
        .map(|item| get_data_and_build_item(client, item));
    let results = futures::future::join_all(futures).await;

    let mut lines = Vec::with_capacity(count);
    for (item, result) in batch.iter().zip(results) {
        match result {
            Ok(built) => {
                trace!("Item processed: {:?}", built.id);
                lines.push(built);
            }
            Err(e) => error!("Error downloading item {}: {e}", item.id),
        }
    }

    append_json_lines(data_file_path, lines)?;
    trace!("Successfully appended {count} items to file.");
    batch.clear();
    Ok(count)
}
