# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
"""Live API drift check.

Fetches a small sample of recent markets from each platform's live API, runs them
through the real downloader and extractor, and reports whether the data still
deserializes and standardizes. This catches external API changes cheaply (a handful
of requests) rather than discovering them part-way through a multi-hour download.

Unlike the golden-sample tests (which use frozen fixtures and catch *our* regressions),
this hits the live APIs and catches *their* drift. Run it manually or on a schedule:

    just live-check                 # all platforms, 5 markets each
    just live-check --platform kalshi --count 8
    uv run scripts/live-check.py    # (loads .env itself)

Exit code is non-zero if any platform shows a hard drift signal (data that no longer
downloads or deserializes), so it can gate CI / cron.
"""

import argparse
import datetime
import json
import os
import re
import subprocess
import sys
import tempfile
import urllib.request
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
PLATFORMS = ["kalshi", "manifold", "metaculus", "polymarket"]


def load_dotenv() -> None:
    """Merge the repo .env into the environment so subprocesses inherit keys."""
    env_path = REPO_ROOT / ".env"
    if not env_path.exists():
        return
    for line in env_path.read_text().splitlines():
        line = line.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        key, _, value = line.partition("=")
        os.environ.setdefault(key.strip(), value.strip().strip('"'))


def fetch_json(url: str, headers: dict | None = None) -> object:
    # Some platforms sit behind Cloudflare and 403 the default urllib User-Agent.
    all_headers = {"User-Agent": "themis-live-check/1.0"}
    all_headers.update(headers or {})
    req = urllib.request.Request(url, headers=all_headers)
    with urllib.request.urlopen(req, timeout=60) as resp:
        return json.load(resp)


def as_float(value: object) -> float:
    try:
        return float(value)  # type: ignore[arg-type]
    except (TypeError, ValueError):
        return 0.0


def sample_markets(platform: str, count: int) -> list[tuple[str, object]]:
    """Return a list of (id, raw_market) tuples to seed the index with."""
    if platform == "kalshi":
        data = fetch_json(
            "https://api.elections.kalshi.com/trade-api/v2/markets"
            "?limit=200&status=settled"
        )
        markets = [
            m
            for m in data.get("markets", [])
            if m.get("status") == "finalized"
            and m.get("result") in ("yes", "no")
            and as_float(m.get("volume_fp")) > 0
        ]
        markets.sort(key=lambda m: as_float(m.get("volume_fp")), reverse=True)
        return [(m["ticker"], m) for m in markets[:count]]

    if platform == "manifold":
        data = fetch_json("https://api.manifold.markets/v0/markets?limit=500")
        markets = [
            m
            for m in data
            if m.get("isResolved")
            and m.get("outcomeType") == "BINARY"
            and m.get("resolution") in ("YES", "NO")
            and as_float(m.get("volume")) > 50
        ]
        markets.sort(key=lambda m: as_float(m.get("volume")), reverse=True)
        return [(m["id"], m) for m in markets[:count]]

    if platform == "metaculus":
        key = os.environ.get("METACULUS_API_KEY", "")
        if not key:
            raise RuntimeError("METACULUS_API_KEY is not set")
        data = fetch_json(
            "https://www.metaculus.com/api/posts/"
            f"?limit={count}&statuses=resolved&with_cp=false&order_by=-published_at"
            "&forecast_type=binary&forecast_type=multiple_choice",
            headers={"Authorization": f"Token {key}"},
        )
        return [(str(p["id"]), p) for p in data.get("results", [])[:count]]

    if platform == "polymarket":
        data = fetch_json("https://clob.polymarket.com/markets?limit=200")
        markets = [
            m
            for m in data.get("data", [])
            if m.get("closed") and m.get("question_id") and m.get("tokens")
        ]
        return [(m["question_id"], m) for m in markets[:count]]

    raise ValueError(f"Unknown platform: {platform}")


def seed_index(cache_dir: Path, platform: str, markets: list[tuple[str, object]]) -> None:
    now = datetime.datetime.now(datetime.timezone.utc).isoformat()
    lines = [
        json.dumps({"id": mid, "last_updated": now, "data": market})
        for mid, market in markets
    ]
    (cache_dir / f"{platform}-index.jsonl").write_text("\n".join(lines) + "\n")


def run(cmd: list[str], cwd: Path) -> subprocess.CompletedProcess:
    return subprocess.run(
        cmd,
        cwd=cwd,
        capture_output=True,
        text=True,
        timeout=600,
        check=False,
    )


def check_platform(platform: str, count: int) -> str:
    """Run the full sample → download → extract pipeline. Returns PASS/WARN/FAIL."""
    print(f"\n=== {platform} ===")
    try:
        markets = sample_markets(platform, count)
    except Exception as e:  # noqa: BLE001 - report and move on
        print(f"  FAIL: could not sample live markets: {e}")
        return "FAIL"
    if not markets:
        print("  WARN: live API returned no usable sample markets")
        return "WARN"
    print(f"  sampled {len(markets)} markets")

    with tempfile.TemporaryDirectory(prefix="themis-live-") as tmp:
        cache_dir = Path(tmp)
        seed_index(cache_dir, platform, markets)

        # Data phase only (the seeded index is treated as already-downloaded).
        dl = run(
            ["cargo", "run", "-qr", "--", "--platform", platform,
             "--output-dir", str(cache_dir), "--log-level", "warn"],
            cwd=REPO_ROOT / "download",
        )
        download_errors = dl.stderr.count("Error downloading")
        data_file = cache_dir / f"{platform}-data.jsonl"
        data_lines = (
            sum(1 for _ in data_file.open()) if data_file.exists() else 0
        )

        if data_lines == 0:
            print(f"  FAIL: downloaded 0 of {len(markets)} markets")
            print(_indent(dl.stderr))
            return "FAIL"

        # Extract offline (no DB): exercises deserialization + standardization.
        ex = run(
            ["cargo", "run", "-qr", "--", "--platform", platform,
             "--directory", str(cache_dir), "--offline", "--log-level", "info"],
            cwd=REPO_ROOT / "extract",
        )
        output = ex.stdout + ex.stderr

        deser_failures = output.count("Failed to deserialize")
        processed = _grab_int(output, r"Processed:\s+(\d+)")
        items_in_file = _grab_int(output, r"Items in file:\s+(\d+)")
        # The "*"-prefixed error types are the ones that indicate a real problem.
        actual_errors = sum(
            int(n) for n in re.findall(r"^\s+\*[^:]+:\s+(\d+)", output, re.MULTILINE)
        )

    print(
        f"  downloaded={data_lines} download_errors={download_errors} "
        f"items_in_file={items_in_file} processed={processed} "
        f"deser_failures={deser_failures} actual_errors={actual_errors}"
    )

    if deser_failures > 0:
        print("  FAIL: data no longer deserializes — likely an API schema change")
        return "FAIL"
    if actual_errors > 0 or download_errors > 0:
        print("  WARN: some items hit data/processing errors — worth a look")
        return "WARN"
    print("  PASS")
    return "PASS"


def _grab_int(text: str, pattern: str) -> int:
    m = re.search(pattern, text)
    return int(m.group(1)) if m else 0


def _indent(text: str) -> str:
    return "\n".join(f"    {line}" for line in text.strip().splitlines()[-15:])


def main() -> int:
    parser = argparse.ArgumentParser(description="Live API drift check.")
    parser.add_argument("--platform", choices=PLATFORMS, help="only check one platform")
    parser.add_argument("--count", type=int, default=5, help="markets to sample per platform")
    args = parser.parse_args()

    load_dotenv()
    platforms = [args.platform] if args.platform else PLATFORMS
    results = {p: check_platform(p, args.count) for p in platforms}

    print("\n=== summary ===")
    for platform, result in results.items():
        print(f"  {platform:11} {result}")

    return 1 if "FAIL" in results.values() else 0


if __name__ == "__main__":
    sys.exit(main())
