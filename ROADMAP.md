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
- [ ] Docs for the new reality — note the Metaculus key requirement and Kalshi
      schema change in the README (template.env already updated).
- [ ] Triage whatever the running downloader turns up (placeholder).

## Stage 2 — Technical debt that makes development easier

- [ ] Cargo workspace + `common` library (2024 edition) — the core of the xray
      refactor, minus the parts we don't want.
- [ ] Set up clippy at a pedantic level
- [ ] Update dependencies (both cargo and astro stuff), update docs to use nvm
- [ ] Golden-sample tests per platform — committed fixtures run by `cargo test` so
      the next API change fails CI instead of a multi-hour download.
- [ ] CI runners (`rust.yml` / `astro.yml` from xray): format + build + test + clippy.
- [ ] Schema-drift detection / validation pass — warn loudly when a platform's data
      stops matching expectations rather than silently dropping markets.

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
