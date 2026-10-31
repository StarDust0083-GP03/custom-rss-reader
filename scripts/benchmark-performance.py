#!/usr/bin/env python3
"""Tag-query benchmark; real databases are opened read-only, never migrated.

python3 scripts/benchmark-performance.py --database ~/.rss-reader/rss_reader.db
Without --database, use a small synthetic database. Output contains aggregate
metrics and query plans only, never article text, URLs, or tag names.
"""

import argparse
from collections import Counter
import json
from pathlib import Path
import re
import sqlite3
import statistics
import time

ROOT = Path(__file__).resolve().parents[1]
BASELINE_TAGS = """SELECT DISTINCT t.name FROM tag_catalog t
JOIN feed_items f ON json_valid(f.tags)
AND EXISTS (SELECT 1 FROM json_each(f.tags) j WHERE j.value = t.name)
ORDER BY t.name"""
BASELINE_CATALOG = """SELECT t.name, COUNT(DISTINCT f.id) AS usage_count
FROM tag_catalog t LEFT JOIN feed_items f ON json_valid(f.tags)
AND EXISTS (SELECT 1 FROM json_each(f.tags) j WHERE j.value = t.name)
GROUP BY t.name ORDER BY t.name"""


def production_queries():
    source = (ROOT / "src-tauri/src/repositories/feed_item_repo.rs").read_text()
    tags_method = source.split("async fn find_all_tags(", 1)[1].split("async fn find_tag_catalog(", 1)[0]
    catalog_method = source.split("async fn find_tag_catalog(", 1)[1].split("async fn find_active_tag_names(", 1)[0]
    tags = re.findall(r'r#"(.*?)"#', tags_method, re.S)
    catalog = re.findall(r'r#"(.*?)"#', catalog_method, re.S)
    assert len(tags) == 2 and len(catalog) == 1, "Production SQL layout changed; update extractor"
    return tags[1], catalog[0]


def synthetic():
    conn = sqlite3.connect(":memory:")
    conn.executescript("""CREATE TABLE feed_items(id INTEGER PRIMARY KEY, subscription_id INTEGER, tags TEXT);
    CREATE TABLE tag_catalog(name TEXT PRIMARY KEY COLLATE NOCASE);""")
    conn.executemany("INSERT INTO tag_catalog VALUES (?)", [(f"tag_{i:03}",) for i in range(100)])
    conn.executemany("INSERT INTO feed_items VALUES (?, ?, ?)", [
        (i, i % 20, json.dumps([f"tag_{i % 99:03}", f"tag_{(i + 1) % 99:03}"]))
        for i in range(1000)
    ])
    return conn


def measure(conn, query, runs, timeout):
    def execute():
        start = time.perf_counter()
        conn.set_progress_handler(lambda: time.perf_counter() - start > timeout, 10000)
        try:
            rows = conn.execute(query).fetchall()
            return rows, (time.perf_counter() - start) * 1000
        finally:
            conn.set_progress_handler(None, 0)

    plan = [row[3] for row in conn.execute("EXPLAIN QUERY PLAN " + query)]
    try:
        rows, _ = execute()  # Warm-up is excluded from the distribution.
        timings = [execute()[1] for _ in range(runs)]
        return {"status": "ok", "rows": len(rows), "runs": runs,
                "median_ms": round(statistics.median(timings), 3),
                "min_ms": round(min(timings), 3), "max_ms": round(max(timings), 3),
                "plan": plan}, rows
    except sqlite3.OperationalError as error:
        if str(error) != "interrupted":
            raise
        return {"status": "timeout", "lower_bound_ms": timeout * 1000, "plan": plan}, None


def reference_counts(conn):
    counts = Counter()
    for (raw,) in conn.execute("SELECT tags FROM feed_items"):
        try:
            tags = json.loads(raw) if raw else []
        except (json.JSONDecodeError, TypeError):
            continue
        if not isinstance(tags, list) or any(not isinstance(tag, str) for tag in tags):
            return None  # Rust regression tests cover legacy scalar/object/number semantics.
        counts.update(set(tags))  # Repeated tags count as one article.
    names = [row[0] for row in conn.execute("SELECT name FROM tag_catalog ORDER BY name")]
    return [(name, counts[name]) for name in names]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--database", type=Path)
    parser.add_argument("--runs", type=int, default=5)
    parser.add_argument("--timeout-seconds", type=float, default=3)
    args = parser.parse_args()
    if args.runs < 1 or args.timeout_seconds <= 0:
        parser.error("runs and timeout must be positive")
    conn = sqlite3.connect(args.database.resolve().as_uri() + "?mode=ro", uri=True) if args.database else synthetic()
    conn.execute("PRAGMA query_only=ON")
    tags, catalog = production_queries()
    reference = reference_counts(conn)
    output = {"dataset": "local-read-only" if args.database else "synthetic",
              "sqlite_version": sqlite3.sqlite_version,
              "items": conn.execute("SELECT COUNT(*) FROM feed_items").fetchone()[0],
              "tags": conn.execute("SELECT COUNT(*) FROM tag_catalog").fetchone()[0],
              "measurements": {}}
    for name, before, after in [("tag_picker", BASELINE_TAGS, tags), ("tag_catalog", BASELINE_CATALOG, catalog)]:
        old, old_rows = measure(conn, before, args.runs, args.timeout_seconds)
        new, new_rows = measure(conn, after, args.runs, args.timeout_seconds)
        assert new_rows, f"{name}: optimized query returned no rows"
        if old_rows is not None:
            assert old_rows == new_rows, f"{name}: baseline and optimized results differ"
        if reference is not None:
            expected = reference if name == "tag_catalog" else [(tag,) for tag, count in reference if count]
            assert new_rows == expected, f"{name}: independent Python reference differs"
        output["measurements"][name] = {"before": old, "after": new,
            "baseline_equal": old_rows == new_rows if old_rows is not None else None,
            "independent_reference_equal": reference is not None}
    conn.close()
    print(json.dumps(output, indent=2))


if __name__ == "__main__":
    main()
