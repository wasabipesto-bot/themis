//! Tools to download and process markets from the Manifold API.

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use log::{debug, error, trace, warn};
use reqwest_middleware::ClientWithMiddleware;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use serde_jsonlines::{append_json_lines, json_lines};
use std::collections::HashSet;
use std::env;
use std::path::Path;
use std::time::Instant;

use super::{IndexItem, Platform};
use crate::util::{
    display_progress, finalize_temp_file, get_id, get_reqwest_client_ratelimited_with_auth,
    get_temp_file_path, send_request,
};

/// Read the optional Manifold API key from the environment and format it as an
/// `Authorization` header value. Returns `None` if unset or blank; Manifold auth
/// is optional but raises rate limits and is needed for some endpoints.
fn manifold_auth_header() -> Option<String> {
    env::var("MANIFOLD_API_KEY")
        .ok()
        .filter(|key| !key.is_empty())
        .map(|key| format!("Key {key}"))
}

const MANIFOLD_API_BASE: &str = "https://api.manifold.markets/v0";
const MANIFOLD_RATELIMIT: usize = 15;
const MANIFOLD_RATELIMIT_MS: u64 = 1000;

/// Format of data saved to JSON
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifoldItem {
    id: String,
    last_updated: DateTime<Utc>,
    lite_market: Value,
    full_market: Value,
    bets: Vec<Value>,
}

/// Download extended data from the `/market/{id}` endpoint.
/// Detect errors and warn but don't stop processing.
async fn get_full_market(client: &ClientWithMiddleware, id: &str) -> Result<Value> {
    trace!("Getting Manifold extended market data for Market {id}");
    let api_url = MANIFOLD_API_BASE.to_owned() + "/market/" + id;

    // submit the request
    send_request(client.get(&api_url)).await.or_else(|err| {
        warn!("Failed to fetch extended market data for Market {id}: {err}");
        trace!("Returning null JSON object instead.");
        Ok(json!(null))
    })
}

/// Download extended data from the `/bets` endpoint.
/// Detect errors and warn but don't stop processing.
/// Source links:
/// https://github.com/manifoldmarkets/manifold/blob/d23cb16f7a2b781d5097648c29d01ed3bebbf55e/common/src/api/schema.ts#L359
/// https://github.com/manifoldmarkets/manifold/blob/d23cb16f7a2b781d5097648c29d01ed3bebbf55e/backend/shared/src/supabase/bets.ts#L34
async fn get_bet_data(client: &ClientWithMiddleware, market_id: &str) -> Result<Vec<Value>> {
    trace!("Getting Manifold bet data for Market {market_id}");
    let api_url = MANIFOLD_API_BASE.to_owned() + "/bets";
    let limit = 1000;
    let mut before: Option<String> = None;
    let mut bets: Vec<Value> = Vec::new();

    // loop until all bets are downloaded
    loop {
        // send the request
        let response = match send_request(
            client
                .get(&api_url)
                .query(&[("contractId", market_id)])
                .query(&[("limit", &limit)])
                .query(&[("before", &before)]) // if value is None, param is not sent
                .query(&[("includeZeroShareRedemptions", true)]), // include price movements on linked MC
        )
        .await
        {
            // if the response came through with no errors, pass along
            Ok(resp) => resp,
            // otherwise, pass an empty vec so we don't break processing
            Err(err) => {
                warn!("Failed to fetch bet data for Market {market_id}: {err}");
                trace!("Returning null JSON object instead.");
                return Ok(vec![json!(null)]);
            }
        };

        // format as an array
        let bet_arr = response
            .as_array()
            .ok_or_else(|| {
                anyhow!(
                    "Could not format API response as array. Response: {:?}",
                    response
                )
            })?
            .to_owned();

        // check the length of the returned array
        // if the length is less than the limit, we've reached the end of the bets
        if bet_arr.len() == limit {
            // update the cursor
            let last_bet = bet_arr
                .last()
                .ok_or_else(|| anyhow!("Bet batch missing items!"))?;
            let last_id = get_id(last_bet)?;
            before = Some(last_id);
            // save the bets
            bets.extend(bet_arr);
        } else {
            // save the bets
            bets.extend(bet_arr);
            // break out
            break;
        }
    }
    trace!(
        "Downloaded {} bet items for market {}",
        bets.len(),
        market_id
    );
    Ok(bets)
}

/// Downloads everything to build a market item.
async fn get_data_and_build_item(
    client: &ClientWithMiddleware,
    item: &IndexItem,
) -> Result<ManifoldItem> {
    let id = item.id.as_str();
    // return the row ready for writing
    Ok(ManifoldItem {
        id: item.id.clone(),
        last_updated: Utc::now(),
        lite_market: item.data.clone(),
        full_market: get_full_market(client, id).await?,
        bets: get_bet_data(client, id).await?,
    })
}

/// Downloads and returns a new index.
pub async fn download_index(index_file_path: &Path) -> Result<()> {
    // set platform
    let platform = Platform::Manifold;

    // get url and client (with optional API key auth)
    let api_url = MANIFOLD_API_BASE.to_owned() + "/markets";
    let client = get_reqwest_client_ratelimited_with_auth(
        MANIFOLD_RATELIMIT,
        MANIFOLD_RATELIMIT_MS,
        manifold_auth_header(),
    )?;

    // write batches to a temp file first, then atomically move it into place
    let temp_file_path = get_temp_file_path(index_file_path);
    let _ = std::fs::remove_file(&temp_file_path);

    // loop through questions endpoint until all are downloaded
    let limit = 1000;
    let mut total = 0usize;
    let mut before: Option<String> = None;
    loop {
        let response = send_request(
            client
                .get(&api_url)
                .query(&[("limit", limit)])
                .query(&[("before", before)]), // if value is None, param is not sent
        )
        .await?;

        let batch = response
            .as_array()
            .map(|response_array| response_array.to_owned())
            .ok_or_else(|| anyhow!("Could not format API reponse as array {}", response))?;

        // build items from batch and stream them straight to the temp file
        let mut items = Vec::with_capacity(batch.len());
        for question in batch.clone() {
            let question_id = get_id(&question)?;
            items.push(IndexItem {
                id: question_id,
                last_updated: Utc::now(),
                data: question,
            });
        }
        append_json_lines(&temp_file_path, items)?;
        total += batch.len();

        // update the cursor or break
        if batch.len() == limit {
            let cursor_some = batch
                .last()
                .map(get_id)
                .transpose()
                .map_err(|e| anyhow!("Failed to get ID for the last batch item: {e}"))?
                .ok_or_else(|| anyhow!("Batch is empty!"))?;
            debug!(
                "Got {} items and new {platform} cursor: {cursor_some}",
                batch.len()
            );
            before = Some(cursor_some);
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
    // Get client (with optional API key auth)
    let platform = Platform::Manifold;
    let client = get_reqwest_client_ratelimited_with_auth(
        MANIFOLD_RATELIMIT,
        MANIFOLD_RATELIMIT_MS,
        manifold_auth_header(),
    )?;

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
