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
- [ ] **Index streaming to disk** — stream the platform index to a temp file and
      read items back during the data phase instead of holding the whole index in
      memory. Kalshi is already at ~40 GB RAM. Reference: the xray approach
      (`get_temp_file_path` / `read_index_item_from_file` / atomic finalize).
- [ ] Run the full pipeline end-to-end against the DB:
      download → extract → group new markets → grade → site-build → deploy.
- [ ] Docs for the new reality — note the Metaculus key requirement and Kalshi
      schema change in the README (template.env already updated).
- [ ] Triage whatever the running downloader turns up (placeholder).

## Stage 2 — Technical debt that makes development easier

- [ ] Cargo workspace + `common` library (2024 edition) — the core of the xray
      refactor, minus the parts we don't want.
- [ ] Golden-sample tests per platform — committed fixtures run by `cargo test` so
      the next API change fails CI instead of a multi-hour download.
- [ ] CI runners (`rust.yml` / `astro.yml` from xray): build + test + clippy.
- [ ] Schema-drift detection / validation pass — warn loudly when a platform's data
      stops matching expectations rather than silently dropping markets.
- [ ] Support remaining Metaculus question types — numeric, date, conditional,
      discrete still route to "not implemented" (multiple-choice already works).
      Overlaps with the non-binary work below.

## Stage 3 — Future / nice-to-have

- [ ] Non-binary market model — separate "market" from "binary contract"
      (xray's `outcome.rs`, neutral aspects, multiple outcomes). Lower priority.
- [ ] Grouper tool improvements / automation — semi-automate the manual linking.
- [ ] API service — the Rocket `api/` crate scaffolded on xray.
- [ ] Point calibration.city at the new API and make improvements there.
- [ ] Decide xray's fate — cherry-pick the wanted pieces (workspace, API, outcome
      model) and drop the xray site, so the branch isn't lingering dead weight.
