# Themis Roadmap

A living checklist, roughly ordered from "necessary to redeploy brier.fyi" to
"deferred technical work" to "future ideas". Items will get reprioritized as the
downloader runs and surfaces new issues — new breakages float to the top of Stage 1.

## Stage 1 — Necessary to redeploy brier.fyi

- [x] Fix platform downloader/extractor API breakages
  - [x] Metaculus now requires an API key (`Token <key>`)
  - [x] Manifold optional API key (`Key <key>`)
  - [x] Kalshi fixed-point/dollars schema migration (downloader + extract)
  - [x] Metaculus `discrete` question variant deserializes
  - [x] Metaculus aggregation changes (`centers` vs `means`, per-question
        `default_aggregation_method` so some questions only expose `unweighted`)
- [x] **Index streaming to disk** — the index now streams to a temp file and is
      finalized atomically; the data phase streams it back lazily (`json_lines`) with
      a membership scan, so the whole index is never held in memory. Improves on the
      xray approach, which re-scanned the index per market (O(n²)); this is O(n).
- [ ] Run the full pipeline end-to-end against the DB:
      download → extract → group new markets → grade → site-build → deploy.
  - [x] Offline extract over the July 2026 test download, all four platforms (2026-10-08).
  - [ ] Load it into a DB restored from the 2026-06-29 backup (without embeddings), grade,
        and build the site locally.
- [ ] Docs for the new reality — note the Metaculus key requirement and Kalshi
      schema change in the README (template.env already updated).
- [ ] Triage from the July 2026 test download:
  - [ ] **Kalshi historical split.** `/markets` and `/markets/trades` now only cover markets
        settled after a rolling cutoff (`/historical/cutoff`, 2026-08-09 as of 2026-10-08); older
        markets moved to `/historical/markets`. The downloader only reads `/markets`, so the July
        data has nothing settled before May 2026 and markets settled between the last DB refresh
        (Nov 2025) and then are in neither. Download from both and deduplicate.
  - [ ] **Kalshi parlays** (`KXMVE*`): about 93% of the July data and 97% of its usable markets
        (~12.4M of 12.7M, averaging 2.5 trades each); 338k are already in the DB. Exclude them at
        index time and remove them from the DB. While there: skip zero-volume markets before
        fetching, and cache event/series lookups (two requests per market today).
  - [ ] **Polymarket index stops after one page.** The loop ends when `batch.len() != limit`, but
        the CLOB API now returns 1000 per page whatever `limit` says (we send 500). Paginate until
        `next_cursor == "LTE="`. The January data is also only 14% of its index (51k of 377k).
  - [ ] Kalshi `result: "scalar"` doesn't deserialize (82,589 lines, 0.3%). Decide whether to
        skip them as not implemented or resolve from the settlement value.
  - [ ] Small ones: Kalshi lines missing `title` (5); Metaculus `details: null` (2), a null f32
        (1) and trade-gap processing errors (10); Polymarket null trade entries (4) and token
        price sums (3); Manifold "resolution is MKT but probability is missing" (4).
  - [x] Extract streams data files instead of loading them whole. Peak memory was ~0.75x the
        file size; now it's set by the largest single line (2.7 GB for one Manifold market,
        under 700 MB elsewhere).
  - [x] `extract --schema-only` no longer needs or touches the database.
  - [x] Clippy lints new in Rust 1.98.

## Stage 2 — Technical debt that makes development easier

- [x] Cargo workspace + `common` library (2024 edition) — the core of the xray
      refactor, minus the parts we don't want.
  - [x] Cargo workspace over common/download/extract/grader (shared package/deps/lints,
        one lockfile, CI on `--workspace`).
  - [x] Edition 2024 across the workspace.
  - [x] `common` library — holds the shared `Platform` enum; download and extract use it
        via extension traits. (Follow-up if wanted: move the PostgREST request plumbing
        shared by extract+grader into `common` — deferred since it's on DB write paths
        that can't be integration-tested without a live database.)
- [x] Set up clippy at a pedantic level — each crate enables `clippy::pedantic` via
      `[lints]` with a curated allow-list for the noisy/subjective lints; CI enforces it
      with `-D warnings`. (Also dropped `lazy_static` in favor of `std::sync::LazyLock`.)
- [x] Update dependencies (both cargo and astro stuff), update docs to use nvm — `cargo
      update` across crates; safe semver `npm update` for site + grouper; nvm in the README.
      Deferred majors (need a coordinated upgrade): astro 5→6, TypeScript 5→6, the
      tailwind/`@tailwindcss/vite` chain (vite-duplication type clash), `@astrojs/svelte` 8,
      jsdom 29.
- [x] Golden-sample tests per platform — committed fixtures + `insta` snapshots run
      by `cargo test` (`extract/tests/golden.rs`); locks deserialization + standardization
      against regressions. (Catching *live* API drift is the separate schema-drift item
      below — these fixtures are frozen. A `refresh-fixtures` script + scheduled run would
      bridge the two.)
- [x] CI runners — `rust.yml` (fmt + clippy `-D warnings` + build + test, matrixed over
      the download/extract/grader crates), `astro.yml` (`astro check` for site + grouper),
      and a scheduled `live-check.yml` (daily live API drift check; needs a
      `METACULUS_API_KEY` repo secret).
- [x] Schema-drift detection / validation pass — warn loudly when a platform's data
      stops matching expectations rather than silently dropping markets.
  - [x] `just live-check` (`scripts/live-check.py`) — samples a few live markets per
        platform, runs the real download + extract, and flags deserialization/processing
        drift. Proactive, cheap, scriptable for cron/CI.
  - [x] Runtime guard in extract — `load_data` bails when a large share of a platform's
        lines fail to deserialize, and the run aborts when the data/processing error rate
        exceeds a threshold, instead of silently dropping most markets.

## Stage 3 — Future / nice-to-have

- [ ] Grouper tool improvements / automation — semi-automate the manual linking.
- [ ] Non-binary market model — separate "market" from "binary contract"
      (xray's `outcome.rs`, neutral aspects, multiple outcomes).
- [ ] Support remaining Metaculus question types — numeric, date, conditional,
      discrete still route to "not implemented" (multiple-choice already works).
- [ ] API service — the Rocket `api/` crate scaffolded on xray.
- [ ] Point calibration.city at the new API and make improvements there.
- [ ] Point brier.fyi site builder at the new API to replace dumb caching
- [ ] Metaforecast-style aggregation and search
- [ ] Add more platforms (see GH issues)
- [ ] Add blog posts
