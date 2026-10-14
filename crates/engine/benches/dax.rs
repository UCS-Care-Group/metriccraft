//! DAX benches (spec N-3; `docs/bench.md`). Run via `cargo xtask bench`, which lists the bench
//! files in `OPENDASH_BENCH_IMPORTS` (lines of `<name>\t<path>`); each `import/parquet_<n>`
//! file is loaded once (Get Data, as the import bench does) and every query below is timed on
//! it: `BENCH dax/<query>_<n> <ns>`, the median of [`RUNS`]. Only query evaluation is timed
//! (parse + evaluate), not the load or building the evaluator's [`Database`]. Without the
//! variable nothing runs.

use std::time::Instant;

use opendash_engine::App;
use opendash_model::dax::{Database, Options, run_query_text};
use serde_json::json;

const RUNS: usize = 5;

/// `(name, query)`: one visual's worth of work each, on the reference dataset's columns; `T` is
/// the loaded Table (named after the file).
const QUERIES: [(&str, &str); 3] = [
    ("sum", "EVALUATE ROW(\"v\", SUM('T'[SalesAmount]))"),
    (
        "calculate_filter",
        "EVALUATE ROW(\"v\", CALCULATE(SUM('T'[SalesAmount]), 'T'[Channel] = \"Store\"))",
    ),
    (
        "distinctcount",
        "EVALUATE ROW(\"v\", DISTINCTCOUNT('T'[CustomerID]))",
    ),
];

fn main() {
    let imports = std::env::var("OPENDASH_BENCH_IMPORTS").unwrap_or_default();
    for (name, path) in imports.lines().filter_map(|l| l.split_once('\t')) {
        let Some(label) = name.strip_prefix("import/parquet_") else {
            continue;
        };
        if let Err(e) = measure(label, path) {
            eprintln!("bench dax/*_{label}: {e}");
            std::process::exit(1);
        }
    }
}

/// Loads `path` and prints each query's median time.
fn measure(label: &str, path: &str) -> Result<(), String> {
    let mut app = App::new();
    let params = json!({"kind": "parquet", "path": path, "item": "Sales"});
    app.run("home.getData", &params)
        .map_err(|e| format!("{path}: {e}"))?;
    let doc = app.document();
    let table = doc.tables.first().ok_or("nothing loaded")?.name();
    let db = Database::new(&doc.model, &doc.tables);
    for (query, text) in QUERIES {
        let text = text.replace("'T'", &format!("'{table}'"));
        let mut times = Vec::with_capacity(RUNS);
        for _ in 0..RUNS {
            let start = Instant::now();
            let result = run_query_text(&db, &text, Options::default())
                .map_err(|e| format!("{query}: {e}"))?;
            times.push(start.elapsed().as_nanos());
            std::hint::black_box(result);
        }
        times.sort_unstable();
        if let Some(ns) = times.get(RUNS / 2) {
            println!("BENCH dax/{query}_{label} {ns}");
        }
    }
    Ok(())
}
