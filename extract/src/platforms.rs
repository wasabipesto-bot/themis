//! Anything that switches based on platform.

use anyhow::{Context, Result, anyhow};
use std::fs::File;
use std::io::{BufRead, BufReader, Lines};
use std::path::{Path, PathBuf};

pub use themis_common::Platform;

use crate::{MarketAndProbs, MarketResult};

pub mod kalshi;
pub mod manifold;
pub mod metaculus;
pub mod polymarket;

/// Deserialized JSONL line straight from the disk. One of any platform type.
/// Boxed due to large size differences between each platform.
#[derive(Clone)]
pub enum PlatformData {
    Kalshi(Box<kalshi::KalshiData>),
    Manifold(Box<manifold::ManifoldData>),
    Metaculus(Box<metaculus::MetaculusData>),
    Polymarket(Box<polymarket::PolymarketData>),
}

/// Extract-specific methods on the shared [`Platform`] enum.
pub trait PlatformExt {
    fn deserialize_line(&self, line: &str) -> Result<PlatformData>;
    fn load_line_match(&self, base_dir: &Path, search: &str) -> Result<PlatformData>;
    fn load_data(&self, base_dir: &Path, fail_fast: &bool) -> Result<Vec<PlatformData>>;
    fn stream_data(&self, base_dir: &Path, fail_fast: bool) -> Result<DataStream>;
    fn standardize(&self, input: PlatformData) -> MarketResult<Vec<MarketAndProbs>>;
}

impl PlatformExt for Platform {
    /// Based on platform, deserialize a line into that platform's datatype.
    fn deserialize_line(&self, line: &str) -> Result<PlatformData> {
        match self {
            Platform::Kalshi => Ok(PlatformData::Kalshi(serde_json::from_str(line)?)),
            Platform::Manifold => Ok(PlatformData::Manifold(serde_json::from_str(line)?)),
            Platform::Metaculus => Ok(PlatformData::Metaculus(serde_json::from_str(line)?)),
            Platform::Polymarket => Ok(PlatformData::Polymarket(serde_json::from_str(line)?)),
        }
    }

    /// Find the first line in the platform data file matching the search term and deserialize it.
    fn load_line_match(&self, base_dir: &Path, search: &str) -> Result<PlatformData> {
        let file_name = format!("{self}-data.jsonl").to_lowercase();
        let data_file_path = base_dir.join(file_name);

        let file = File::open(&data_file_path)
            .with_context(|| format!("Failed to open file: {}", data_file_path.display()))?;
        let reader = BufReader::new(file);

        for (line_number, line) in reader.lines().enumerate() {
            match line {
                Ok(line_content) => {
                    if line_content.contains(search) {
                        return self.deserialize_line(&line_content).with_context(|| {
                            format!("Failed to deserialize matching line {}", line_number + 1)
                        });
                    }
                }
                Err(err) => {
                    log::error!("Failed to read line {}: {}", line_number + 1, err);
                }
            }
        }

        anyhow::bail!("No line found containing search term: {search}")
    }

    /// Find the appropriate data file based on platform name, then load and deserialize all lines.
    /// Holds every item in memory, so only use it on small files (tests, fixtures); real
    /// cache files should go through [`PlatformExt::stream_data`].
    fn load_data(&self, base_dir: &Path, fail_fast: &bool) -> Result<Vec<PlatformData>> {
        self.stream_data(base_dir, *fail_fast)?.collect()
    }

    /// Open the platform's data file and deserialize it lazily, one line at a time, so
    /// memory use stays flat no matter how large the cache file is.
    fn stream_data(&self, base_dir: &Path, fail_fast: bool) -> Result<DataStream> {
        let file_name = format!("{self}-data.jsonl").to_lowercase();
        let path = base_dir.join(file_name);
        let file = File::open(&path)
            .with_context(|| format!("Failed to open file: {}", path.display()))?;
        Ok(DataStream {
            platform: *self,
            path,
            lines: BufReader::with_capacity(READ_BUFFER_BYTES, file).lines(),
            line_number: 0,
            loaded: 0,
            failed: 0,
            fail_fast,
            finished: false,
        })
    }

    /// Call each platform's standardize function.
    fn standardize(&self, input_unsorted: PlatformData) -> MarketResult<Vec<MarketAndProbs>> {
        match input_unsorted {
            PlatformData::Kalshi(input) => kalshi::standardize(&input),
            PlatformData::Manifold(input) => manifold::standardize(&input),
            PlatformData::Metaculus(input) => metaculus::standardize(&input),
            PlatformData::Polymarket(input) => polymarket::standardize(&input),
        }
    }
}

/// Read buffer for cache files. Big reads matter when the cache sits on a network mount.
const READ_BUFFER_BYTES: usize = 1 << 20;

/// Lazily deserializes a platform data file, one line per item.
///
/// Lines that fail to deserialize are logged and skipped. The stream yields an `Err` and
/// then ends on an I/O error, on the first bad line when `fail_fast` is set, or after the
/// last line if too many lines failed (see [`check_schema_drift`]).
pub struct DataStream {
    platform: Platform,
    path: PathBuf,
    lines: Lines<BufReader<File>>,
    line_number: usize,
    loaded: usize,
    failed: usize,
    fail_fast: bool,
    finished: bool,
}

impl Iterator for DataStream {
    type Item = Result<PlatformData>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.finished {
            return None;
        }
        loop {
            match self.lines.next() {
                Some(Ok(line_content)) => {
                    self.line_number += 1;
                    match self.platform.deserialize_line(&line_content) {
                        Ok(item) => {
                            self.loaded += 1;
                            return Some(Ok(item));
                        }
                        Err(err) => {
                            let err_msg = deserialize_error_message(
                                &self.path,
                                self.line_number,
                                &line_content,
                                &err,
                            );
                            log::error!("{err_msg}");
                            self.failed += 1;
                            if self.fail_fast {
                                self.finished = true;
                                return Some(Err(anyhow!(err_msg)));
                            }
                        }
                    }
                }
                Some(Err(err)) => {
                    self.line_number += 1;
                    let err_msg = format!(
                        "Failed to deserialize file {} line {}: {}",
                        self.path.display(),
                        self.line_number,
                        err
                    );
                    log::error!("{err_msg}");
                    self.finished = true;
                    return Some(Err(anyhow!(err_msg)));
                }
                None => {
                    self.finished = true;
                    let total_lines = self.loaded + self.failed;
                    return check_schema_drift(self.platform, self.failed, total_lines)
                        .err()
                        .map(Err);
                }
            }
        }
    }
}

/// Schema-drift guard: a few unparseable lines happen, but if a large share of a
/// platform's data no longer deserializes, the API schema has almost certainly
/// changed and we should stop loudly rather than silently dropping most markets.
/// The file is streamed, so the items before this point have already been processed;
/// the run still fails.
fn check_schema_drift(platform: Platform, failed_lines: usize, total_lines: usize) -> Result<()> {
    if failed_lines > 0 {
        let failure_rate = failed_lines as f64 / total_lines as f64;
        log::warn!(
            "{platform}: {failed_lines}/{total_lines} lines failed to deserialize ({:.1}%).",
            failure_rate * 100.0
        );
        const DRIFT_MIN_LINES: usize = 20;
        const DRIFT_RATE_THRESHOLD: f64 = 0.25;
        if total_lines >= DRIFT_MIN_LINES && failure_rate >= DRIFT_RATE_THRESHOLD {
            anyhow::bail!(
                "{platform}: {:.1}% of lines failed to deserialize — the API schema has \
                 likely changed. Halting instead of dropping most markets. Run \
                 `just live-check` to confirm and update the relevant structs.",
                failure_rate * 100.0
            );
        }
    }
    Ok(())
}

/// Build the error message for a line that failed to deserialize, with the text around
/// the failing column when serde reports one.
fn deserialize_error_message(
    path: &Path,
    line_number: usize,
    line_content: &str,
    err: &anyhow::Error,
) -> String {
    let mut err_msg = format!(
        "Failed to deserialize file {} line {}: {}",
        path.display(),
        line_number,
        err
    );

    // Try to extract column number from error message and show context
    let err_str = err.to_string();

    // Extract column number using simple string parsing
    if let Some(column_start) = err_str.find("column ") {
        let column_substr = &err_str[column_start + 7..];
        // Find where the number ends - look for first non-digit or end of string
        let column_end = column_substr
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(column_substr.len());
        let column_str = &column_substr[..column_end];

        if column_str.is_empty() {
            err_msg.push_str("- Empty column number found in error message.");
        } else if let Ok(column) = column_str.parse::<usize>() {
            if column > 0 && column <= line_content.len() {
                let char_pos = column - 1; // Convert to 0-based index

                // Handle UTF-8 properly by working with char boundaries
                let chars: Vec<char> = line_content.chars().collect();
                if char_pos < chars.len() {
                    let start_char_pos = char_pos.saturating_sub(25);
                    let end_char_pos = std::cmp::min(char_pos + 25, chars.len());

                    let before: String = chars[start_char_pos..char_pos].iter().collect();
                    let at_pos = chars[char_pos];
                    let after: String = chars[char_pos + 1..end_char_pos].iter().collect();

                    err_msg.push_str(&format!(
                        " - Context around column {column}: `{before}[{at_pos}]{after}`"
                    ));
                } else {
                    err_msg.push_str(&format!(
                        " - Additionally, column {} exceeds character count (chars: {}).",
                        column,
                        chars.len()
                    ));
                }
            } else {
                err_msg.push_str(&format!(
                    " - Additionally, column {} out of bounds (line length: {}).",
                    column,
                    line_content.len()
                ));
            }
        } else {
            err_msg.push_str(&format!(
                " - Additionally, could not parse column number from: '{column_str}'"
            ));
        }
    }
    err_msg
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_serde_json_error_format() {
        // Test what serde_json error messages look like with various malformed JSON
        let test_cases = [
            r#"{"valid": "json", "but": "missing quote here, "invalid": "field"}"#,
            r#"{"valid": true, "another": false, "bad_comma":, "field": "value"}"#,
            r#"{"unclosed": "string"#,
            r#"{"trailing": "comma",}"#,
        ];

        for (i, malformed_json) in test_cases.iter().enumerate() {
            println!("Test case {}: {}", i + 1, malformed_json);
            let result: Result<serde_json::Value, _> = serde_json::from_str(malformed_json);
            if let Err(err) = result {
                println!("  Serde JSON error: {err}");

                // Test our parsing logic
                let err_str = err.to_string();
                if let Some(column_start) = err_str.find("column ") {
                    let column_substr = &err_str[column_start + 7..];
                    let column_end = column_substr
                        .find(|c: char| !c.is_ascii_digit())
                        .unwrap_or(column_substr.len());
                    let column_str = &column_substr[..column_end];

                    if !column_str.is_empty()
                        && let Ok(column) = column_str.parse::<usize>()
                    {
                        println!("  Extracted column: {column}");

                        // Show context like our real code does
                        if column > 0 && column <= malformed_json.len() {
                            let chars: Vec<char> = malformed_json.chars().collect();
                            if column - 1 < chars.len() {
                                let char_pos = column - 1;
                                let start_char_pos = char_pos.saturating_sub(25);
                                let end_char_pos = std::cmp::min(char_pos + 25, chars.len());

                                let before: String =
                                    chars[start_char_pos..char_pos].iter().collect();
                                let at_pos = chars[char_pos];
                                let after: String =
                                    chars[char_pos + 1..end_char_pos].iter().collect();

                                println!("  Context: \"{before}[{at_pos}]{after}\"");
                            }
                        }
                    }
                } else {
                    println!("  No column information found");
                }
            }
            println!();
        }
    }
}
