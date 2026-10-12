//! Import benches (spec N-2, N-8; ADR 0005). Run via `cargo xtask bench`, which generates the
//! files and lists them in `OPENDASH_BENCH_IMPORTS` (lines of `<name>\t<path>`); without it only
//! the idle bench runs.
//!
//! Each measurement is a fresh process (this binary again, with `OPENDASH_BENCH_CASE` set), so
//! its peak RSS is the import's own: Get Data (`home.getData`, the real path: connect, run, load)
//! on a new App. Prints the median of [`RUNS`]:
//! - `BENCH <name> <ns>`: the Get Data command;
//! - `BENCHMEM <name>/peak <bytes>`: the process's peak RSS (`VmHWM`);
//! - `BENCHMEM <name>/steady <bytes>`: RSS afterwards, the table held (`VmRSS`);
//! - `BENCHMEM <name>/table <bytes>`: the loaded table's column buffers.
//!
//! `engine/idle/steady` is a new App's RSS (N-8's idle floor before there is a window). Memory
//! comes from `/proc/self/status`; elsewhere only the time is printed.

use std::process::Command;
use std::time::Instant;

use opendash_engine::App;
use serde_json::json;

const CASE: &str = "OPENDASH_BENCH_CASE";
const RUNS: usize = 3;

fn main() {
    if let Ok(path) = std::env::var(CASE) {
        if let Err(e) = child(&path) {
            eprintln!("{e}");
            std::process::exit(1);
        }
        return;
    }
    let imports = std::env::var("OPENDASH_BENCH_IMPORTS").unwrap_or_default();
    let cases = std::iter::once(("engine/idle", ""))
        .chain(imports.lines().filter_map(|l| l.split_once('\t')));
    for (name, path) in cases {
        if let Err(e) = measure(name, path) {
            eprintln!("bench {name}: {e}");
            std::process::exit(1);
        }
    }
}

/// Runs the case [`RUNS`] times, each in a new process, and prints the medians.
fn measure(name: &str, path: &str) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut runs: Vec<Vec<Option<f64>>> = Vec::new();
    for _ in 0..RUNS {
        let out = Command::new(&exe)
            .env(CASE, path)
            .output()
            .map_err(|e| e.to_string())?;
        let text = String::from_utf8_lossy(&out.stdout);
        let line = text.lines().find_map(|l| l.strip_prefix("RESULT "));
        let (true, Some(line)) = (out.status.success(), line) else {
            return Err(String::from_utf8_lossy(&out.stderr).into_owned());
        };
        runs.push(line.split(' ').map(|v| v.parse().ok()).collect());
    }
    let median = |i: usize| {
        let mut v: Vec<f64> = runs
            .iter()
            .filter_map(|r| r.get(i).copied().flatten())
            .collect();
        v.sort_by(f64::total_cmp);
        v.get(v.len() / 2).copied()
    };
    if path.is_empty() {
        if let Some(rss) = median(2) {
            println!("BENCHMEM {name}/steady {rss:.0}");
        }
        return Ok(());
    }
    if let Some(ns) = median(0) {
        println!("BENCH {name} {ns:.0}");
    }
    let mut any = false;
    for (i, what) in [(1, "peak"), (2, "steady"), (3, "table")] {
        if let Some(bytes) = median(i) {
            println!("BENCHMEM {name}/{what} {bytes:.0}");
            any = true;
        }
    }
    if !any {
        eprintln!("bench {name}: memory unavailable on this OS");
    }
    Ok(())
}

/// One measurement: prints `RESULT <ns> <peak> <steady> <table>` (`-` where unknown).
fn child(path: &str) -> Result<(), String> {
    let mut app = App::new();
    let (mut ns, mut table) = (None, None);
    if !path.is_empty() {
        let kind = path.rsplit('.').next().unwrap_or_default();
        let params = json!({"kind": kind, "path": path, "item": "Sales"});
        let start = Instant::now();
        app.run("home.getData", &params)
            .map_err(|e| format!("{path}: {e}"))?;
        ns = Some(start.elapsed().as_nanos());
        table = app.document().tables.first().map(|t| t.memory());
    }
    let show = |v: Option<String>| v.unwrap_or_else(|| "-".into());
    println!(
        "RESULT {} {} {} {}",
        show(ns.map(|v| v.to_string())),
        show(status_bytes("VmHWM:").map(|v| v.to_string())),
        show(status_bytes("VmRSS:").map(|v| v.to_string())),
        show(table.map(|v| v.to_string())),
    );
    Ok(())
}

/// A `/proc/self/status` field in bytes (Linux only).
fn status_bytes(key: &str) -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let kb = status
        .lines()
        .find_map(|l| l.strip_prefix(key))?
        .split_whitespace()
        .next()?
        .parse::<u64>()
        .ok()?;
    kb.checked_mul(1024)
}
