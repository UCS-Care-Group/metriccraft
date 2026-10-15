//! Repo automation: `cargo xtask <task>`.
//!
//! Tasks: `ci` (everything below, in order), `fmt`, `clippy`, `test`, `layers`, `assets`, `wasm`,
//! `bench [--save] [--reference]`, `snapshot [--update]`, `ui-bench`, `dist [--target T]…`,
//! `parity [--update | --export]`; and
//! `dataset [--rows N] [--out DIR] [--csv]` (the reference dataset, `docs/bench.md`).

use std::collections::BTreeSet;
use std::path::Path;
use std::process::{Command, ExitCode};

mod dataset;
mod dist;
mod memcap;

type Result<T = ()> = std::result::Result<T, String>;

/// Layer of every workspace crate (`docs/plan/0001-mvp.md` §1). A crate may depend only on
/// crates in the same or a lower layer. A crate missing here fails `layers`.
const LAYERS: &[(&str, u8)] = &[
    ("metriccraft-core", 0),
    ("metriccraft-dax", 1),
    ("metriccraft-m", 1),
    ("metriccraft-data", 2),
    ("metriccraft-model", 2),
    ("metriccraft-file", 2),
    ("metriccraft-visuals", 3),
    ("metriccraft-engine", 5),
    ("metriccraft-ui-egui", 6),
    ("metriccraft-mcp", 6),
    ("metriccraft", 7),
    ("metriccraft-cli", 7),
    ("metriccraft-web", 7),
    ("xtask", 8),
];

/// Crates that only the UI layer (6) and above may depend on: the UI crate is swappable.
const UI_ONLY: &[&str] = &[
    "egui",
    "eframe",
    "winit",
    "rfd",
    "egui_dock",
    "egui_kittest",
];

/// Native-only binaries left out of the wasm build.
const NOT_WASM: &[&str] = &["metriccraft", "metriccraft-cli", "xtask"];

/// File extensions that count as assets and need an `ATTRIBUTION.md` row. Text data we write
/// ourselves (JSON, CSV, parity values) is not an asset; binary media, fonts and documents are.
const ASSET_EXTS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "webp", "bmp", "ico", "icns", "svg", "ttf", "otf", "woff",
    "woff2", "wav", "mp3", "ogg", "xlsx", "parquet", "pbix", "pbit", "mcraft",
];

fn main() -> ExitCode {
    let task = std::env::args().nth(1).unwrap_or_default();
    let result = match task.as_str() {
        "ci" => ci(),
        "fmt" => fmt(),
        "clippy" => clippy(),
        "test" => test_with(&std::env::args().skip(2).collect::<Vec<_>>()),
        "layers" => layers(),
        "assets" => assets(),
        "strings" => strings(),
        "wasm" => wasm(),
        "dataset" => dataset::run(&std::env::args().skip(2).collect::<Vec<_>>()),
        "bench" => bench(std::env::args().any(|a| a == "--save")),
        "snapshot" => snapshot(std::env::args().any(|a| a == "--update")),
        "ui-bench" => ui_bench(),
        "dist" => dist::run(&std::env::args().skip(2).collect::<Vec<_>>()),
        "parity" if std::env::args().any(|a| a == "--export") => parity_export(),
        "parity" => parity(
            std::env::args().any(|a| a == "--update"),
            std::env::args().any(|a| a == "--pending"),
        ),
        _ => {
            Err("usage: cargo xtask <ci|fmt|clippy|test|layers|assets|strings|wasm|bench [--save]|snapshot [--update]|ui-bench|dist|parity>".into())
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("xtask {task}: {e}");
            ExitCode::FAILURE
        }
    }
}

fn ci() -> Result {
    for (name, step) in [
        ("fmt", fmt as fn() -> Result),
        ("layers", layers),
        ("assets", assets),
        ("strings", strings),
        ("clippy", clippy),
        ("test", test),
        ("wasm", wasm),
        ("bench", || bench(false)),
    ] {
        eprintln!("== {name}");
        step().map_err(|e| format!("{name}: {e}"))?;
    }
    eprintln!("== ci ok");
    Ok(())
}

fn fmt() -> Result {
    cargo(&["fmt", "--all", "--check"])
}

fn clippy() -> Result {
    cargo(&[
        "clippy",
        "--workspace",
        "--all-targets",
        "--",
        "-D",
        "warnings",
    ])
}

fn test() -> Result {
    test_with(&[])
}

/// `cargo test --workspace` (or `cargo test <args>` when given), every test process under an
/// address-space cap (`memcap`) so a runaway test fails instead of OOM-killing the host.
/// Runner via `prlimit` (not a `ulimit` around cargo) so rustc/the linker stay uncapped.
fn test_with(args: &[String]) -> Result {
    let mut full = vec!["test"];
    if args.is_empty() {
        // Every crate's failures in one run, not just the first failing crate's.
        full.extend(["--workspace", "--no-fail-fast"]);
    }
    full.extend(args.iter().map(String::as_str));
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let mut cmd = Command::new(cargo);
    cmd.args(&full);
    if let Some((name, value)) = memcap::runner_env() {
        cmd.env(name, value);
    }
    let status = cmd.status().map_err(|e| format!("running cargo: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("`cargo {}` failed", full.join(" ")))
    }
}

/// Runs every snapshot test (ADR 0007), writing PNGs to `observations/snapshots/` to look at;
/// `--update` re-records the committed text snapshots.
fn snapshot(update: bool) -> Result {
    let png = std::env::current_dir()
        .map_err(|e| e.to_string())?
        .join("observations/snapshots");
    let mut cmd = Command::new("cargo");
    cmd.args(["test", "--workspace", "snapshot"])
        .env("MCRAFT_SNAPSHOT_PNG", &png);
    if update {
        cmd.env("MCRAFT_SNAPSHOT_UPDATE", "1");
    }
    let status = cmd.status().map_err(|e| e.to_string())?;
    if !status.success() {
        return Err("snapshot tests failed".into());
    }
    println!("PNGs in {}", png.display());
    Ok(())
}

/// Spec N-8 budgets for the blank desktop app (ADR 0008).
const COLD_START_MS: u128 = 1000;
/// Idle memory of the app's own (anonymous resident pages): what Power BI's ~451 MB was read
/// as (Task Manager's private working set). Shared libraries, the graphics driver's above all,
/// are reported but not budgeted.
const IDLE_PRIVATE_BYTES: u64 = 100 << 20;

/// Starts the release desktop app on a fresh profile (`MCRAFT_UI_BENCH=1`, see its `Bench`):
/// cold start is spawn to first frame on screen, then idle memory after two seconds; both
/// against the N-8 budgets. Needs a display: under `xvfb-run` where there is none, skipped
/// where neither is available (so it can sit in any CI job).
fn ui_bench() -> Result {
    let display = ["DISPLAY", "WAYLAND_DISPLAY"]
        .iter()
        .any(|v| std::env::var_os(v).is_some_and(|d| !d.is_empty()));
    let xvfb = Command::new("xvfb-run").arg("--help").output().is_ok();
    if !display && !xvfb {
        println!("ui-bench: skipped (no display and no xvfb-run)");
        return Ok(());
    }
    cargo(&["build", "--release", "-p", "metriccraft", "--quiet"])?;
    let target = std::env::var("CARGO_TARGET_DIR").unwrap_or_else(|_| "target".into());
    let bin = Path::new(&target).join("release").join("metriccraft");
    let profile = std::env::temp_dir().join(format!("mcraft-ui-bench-{}", std::process::id()));
    std::fs::create_dir_all(&profile).map_err(|e| format!("{}: {e}", profile.display()))?;
    let mut cmd = if display {
        Command::new(&bin)
    } else {
        let mut c = Command::new("xvfb-run");
        c.args(["-a"]).arg(&bin);
        c
    };
    for v in [
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_STATE_HOME",
        "XDG_CACHE_HOME",
    ] {
        cmd.env(v, &profile);
    }
    let start = std::time::Instant::now();
    let mut child = cmd
        .env("MCRAFT_UI_BENCH", "1")
        .stdout(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("starting {}: {e}", bin.display()))?;
    let stdout = child.stdout.take().ok_or("no stdout")?;
    let (mut cold, mut private, mut rss) = (None, None, None);
    for line in std::io::BufRead::lines(std::io::BufReader::new(stdout)).map_while(|l| l.ok()) {
        if line == "BENCH ui/first_frame" {
            cold = Some(start.elapsed().as_millis());
        } else if let Some(n) = line.strip_prefix("BENCH ui/idle_private ") {
            private = n.trim().parse::<u64>().ok();
        } else if let Some(n) = line.strip_prefix("BENCH ui/idle_rss ") {
            rss = n.trim().parse::<u64>().ok();
        }
    }
    let status = child.wait();
    let _ = std::fs::remove_dir_all(&profile);
    let cold = cold.ok_or_else(|| format!("the app never showed a frame (exit: {status:?})"))?;
    // Timing is gated only on a machine with a bench baseline (the reference machine), as in
    // `bench`: hosted CI draws with software OpenGL. Memory is gated everywhere.
    let machine = machine_id();
    let timed = Path::new(&format!("bench/{machine}.json")).exists();
    let mut problems = Vec::new();
    if timed {
        println!("  ui/cold_start: {cold} ms (budget {COLD_START_MS} ms)");
        if cold >= COLD_START_MS {
            problems.push(format!("cold start {cold} ms"));
        }
    } else {
        println!("  ui/cold_start: {cold} ms (no {machine} baseline: not gated)");
    }
    match private {
        Some(bytes) => {
            let budget = IDLE_PRIVATE_BYTES >> 20;
            println!("  ui/idle_private: {} MB (budget {budget} MB)", bytes >> 20);
            if bytes > IDLE_PRIVATE_BYTES {
                problems.push(format!("idle memory {} MB", bytes >> 20));
            }
        }
        None => println!("  ui/idle_private: not measured on this platform"),
    }
    if let Some(rss) = rss {
        println!(
            "  ui/idle_rss: {} MB (with shared libraries; not budgeted)",
            rss >> 20
        );
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(format!("over budget: {}", problems.join(", ")))
    }
}

fn wasm() -> Result {
    let mut args = vec!["build", "--workspace", "--target", "wasm32-unknown-unknown"];
    for name in NOT_WASM {
        args.extend(["--exclude", name]);
    }
    cargo(&args)
}

fn cargo(args: &[&str]) -> Result {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let status = Command::new(cargo)
        .args(args)
        .status()
        .map_err(|e| format!("running cargo: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("`cargo {}` failed", args.join(" ")))
    }
}

fn output(cmd: &str, args: &[&str]) -> Result<String> {
    let out = Command::new(cmd)
        .args(args)
        .output()
        .map_err(|e| format!("running {cmd}: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "`{cmd} {}` failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    String::from_utf8(out.stdout).map_err(|e| format!("{cmd} output: {e}"))
}

fn report(errors: Vec<String>) -> Result {
    if errors.is_empty() {
        return Ok(());
    }
    for e in &errors {
        eprintln!("  {e}");
    }
    Err(format!("{} problem(s)", errors.len()))
}

// ---- layers ----

/// A workspace crate and its normal + build dependencies (dev-dependencies never ship).
struct Krate {
    name: String,
    deps: Vec<String>,
}

fn layers() -> Result {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let json = output(&cargo, &["metadata", "--format-version", "1", "--no-deps"])?;
    let meta: serde_json::Value =
        serde_json::from_str(&json).map_err(|e| format!("cargo metadata: {e}"))?;
    let krates = meta["packages"]
        .as_array()
        .ok_or("cargo metadata: no packages")?
        .iter()
        .map(|p| Krate {
            name: p["name"].as_str().unwrap_or_default().to_owned(),
            deps: p["dependencies"]
                .as_array()
                .map(Vec::as_slice)
                .unwrap_or_default()
                .iter()
                .filter(|d| d["kind"].as_str() != Some("dev"))
                .filter_map(|d| d["name"].as_str().map(str::to_owned))
                .collect(),
        })
        .collect::<Vec<_>>();
    report(check_layers(&krates))
}

fn layer_of(name: &str) -> Option<u8> {
    LAYERS.iter().find(|(n, _)| *n == name).map(|(_, l)| *l)
}

fn check_layers(krates: &[Krate]) -> Vec<String> {
    let mut errors = Vec::new();
    for k in krates {
        let Some(own) = layer_of(&k.name) else {
            errors.push(format!("{}: not in the xtask LAYERS table", k.name));
            continue;
        };
        for dep in &k.deps {
            if let Some(dep_layer) = layer_of(dep) {
                if dep_layer > own {
                    errors.push(format!(
                        "{} (L{own}) depends on {dep} (L{dep_layer}), a higher layer",
                        k.name
                    ));
                }
            } else if own < 6 && UI_ONLY.contains(&dep.as_str()) {
                errors.push(format!(
                    "{} (L{own}) depends on {dep}; only L6+ may use UI crates",
                    k.name
                ));
            }
        }
    }
    errors
}

// ---- assets ----

fn assets() -> Result {
    let files = output(
        "git",
        &["ls-files", "--cached", "--others", "--exclude-standard"],
    )?;
    let attribution = std::fs::read_to_string("ATTRIBUTION.md")
        .map_err(|e| format!("reading ATTRIBUTION.md: {e}"))?;
    report(check_assets(files.lines(), &attribution, |p| {
        Path::new(p).exists()
    }))
}

/// Widget calls whose text must come from the string catalogue (spec A-2).
const WIDGET_CALLS: &[&str] = &[
    "label(",
    "button(",
    "heading(",
    "strong(",
    "selectable_label(",
    "selectable_value(",
    "checkbox(",
    "hint_text(",
    "on_hover_text(",
    "RichText::new(",
    "Button::new(",
    "Label::new(",
    "menu_button(",
    "with_title(",
];

/// UI strings: no letters in a string literal passed to a widget in `crates/ui-egui/src`, outside
/// `strings.rs` (the catalogue) and test code.
fn strings() -> Result {
    let mut errs = Vec::new();
    let dir = Path::new("crates/ui-egui/src");
    let entries = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "rs") || path.ends_with("strings.rs") {
            continue;
        }
        let text =
            std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        errs.extend(literal_widget_text(&text).into_iter().map(|n| {
            format!(
                "{}:{n}: widget text not from the string catalogue",
                path.display()
            )
        }));
    }
    report(errs)
}

/// Line numbers (1-based) of widget calls given a literal with letters, before `#[cfg(test)]`.
fn literal_widget_text(src: &str) -> Vec<usize> {
    let mut out = Vec::new();
    for (i, line) in src.lines().enumerate() {
        if line.trim_start().starts_with("#[cfg(test)]") {
            break;
        }
        if line.trim_start().starts_with("//") {
            continue;
        }
        for call in WIDGET_CALLS {
            let mut rest = line;
            while let Some(at) = rest.find(call) {
                rest = rest.get(at + call.len()..).unwrap_or_default();
                let args = rest.split(')').next().unwrap_or_default();
                let lettered = args
                    .split('"')
                    .skip(1)
                    .step_by(2)
                    .any(|lit| lit.chars().any(char::is_alphabetic));
                if lettered && !out.contains(&(i + 1)) {
                    out.push(i + 1);
                }
            }
        }
    }
    out
}

fn is_asset(path: &str) -> bool {
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    ASSET_EXTS.contains(&ext.as_str())
}

/// Asset paths listed in the first column of `ATTRIBUTION.md`'s table.
fn attributed(attribution: &str) -> BTreeSet<String> {
    attribution
        .lines()
        .filter_map(|l| l.trim().strip_prefix('|'))
        .filter_map(|l| l.split('|').next())
        .map(|cell| cell.trim().trim_matches('`').to_owned())
        .filter(|cell| !cell.is_empty() && cell != "Asset" && !cell.starts_with('-'))
        .collect()
}

fn check_assets<'a>(
    files: impl Iterator<Item = &'a str>,
    attribution: &str,
    exists: impl Fn(&str) -> bool,
) -> Vec<String> {
    let listed = attributed(attribution);
    let mut errors = Vec::new();
    for f in files.filter(|f| is_asset(f)) {
        if !listed.contains(f) {
            errors.push(format!("{f}: asset without an ATTRIBUTION.md row"));
        }
    }
    for row in &listed {
        if !exists(row) {
            errors.push(format!("{row}: ATTRIBUTION.md row for a missing file"));
        }
    }
    errors
}

// ---- bench ----

/// A result more than this factor slower than the machine's baseline fails.
const BENCH_TOLERANCE: f64 = 1.25;
/// A result using more than this factor of the baseline's memory fails (ADR 0005).
const BENCH_MEMORY_TOLERANCE: f64 = 1.10;
/// Rows of the import benches' files: Parquet, CSV, XLSX (`docs/bench.md`).
const BENCH_IMPORTS: [(&str, usize); 3] =
    [("parquet", 1_000_000), ("csv", 200_000), ("xlsx", 50_000)];

/// Runs every `harness = false` bench, compares with `bench/<machine>.json` and, with `--save`,
/// records the results as that machine's new baseline (merged into it, so a default run keeps
/// the `--reference` results). Machines without a baseline only report. `--reference` adds the
/// 10M-row Parquet import (spec N-2), from `observations/reference/`.
fn bench(save: bool) -> Result {
    let reference = std::env::args().any(|a| a == "--reference");
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let out = Command::new(cargo)
        .args(["bench", "--workspace", "--benches", "--quiet"])
        .env("METRICCRAFT_BENCH_IMPORTS", bench_imports(reference)?)
        .output()
        .map_err(|e| format!("running cargo bench: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "`cargo bench` failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    let out = String::from_utf8_lossy(&out.stdout);
    let times = parse_bench(&out, "BENCH ");
    let memory = parse_bench(&out, "BENCHMEM ");
    if times.is_empty() {
        return Err("no BENCH lines in `cargo bench` output".into());
    }
    let machine = machine_id();
    let path = format!("bench/{machine}.json");
    let baseline = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .unwrap_or_default();
    let mut errors = Vec::new();
    for (key, results, tolerance, unit, worse) in [
        ("results", &times, BENCH_TOLERANCE, "ns", "slower"),
        (
            "memory",
            &memory,
            BENCH_MEMORY_TOLERANCE,
            "bytes",
            "the memory of",
        ),
    ] {
        for (name, value) in results {
            match baseline[key][name].as_f64() {
                Some(was) => {
                    let ratio = value / was;
                    eprintln!("  {name}: {value:.1} {unit} (baseline {was:.1}, x{ratio:.2})");
                    if ratio > tolerance && !save {
                        errors.push(format!(
                            "{name}: x{ratio:.2} {worse} the {machine} baseline"
                        ));
                    }
                }
                None => eprintln!("  {name}: {value:.1} {unit} (no {machine} baseline)"),
            }
        }
    }
    if save {
        let merged = |key: &str, results: &[(String, f64)]| {
            let mut map = baseline[key].as_object().cloned().unwrap_or_default();
            for (n, v) in results {
                map.insert(n.clone(), serde_json::json!(v));
            }
            serde_json::Value::Object(map)
        };
        let json = serde_json::json!({
            "machine": machine_info(&machine),
            "results": merged("results", &times),
            "memory": merged("memory", &memory),
        });
        let text = serde_json::to_string_pretty(&json).map_err(|e| e.to_string())?;
        std::fs::create_dir_all("bench").map_err(|e| format!("creating bench/: {e}"))?;
        std::fs::write(&path, text + "\n").map_err(|e| format!("writing {path}: {e}"))?;
        eprintln!("  saved {path}");
    }
    report(errors)
}

/// The import bench files, generated once per size into `<target>/bench-data/` (the reference
/// file into `observations/reference/`), as `METRICCRAFT_BENCH_IMPORTS` lines `<name>\t<path>`.
fn bench_imports(reference: bool) -> Result<String> {
    let target = std::env::var("CARGO_TARGET_DIR").unwrap_or_else(|_| "target".into());
    let cache = Path::new(&target).join(format!("bench-data/v{}", dataset::VERSION));
    let mut files = Vec::new();
    for (kind, rows) in BENCH_IMPORTS {
        let write = match kind {
            "csv" => dataset::write_csv,
            "xlsx" => dataset::write_xlsx,
            _ => dataset::write_parquet,
        };
        files.push((kind, rows, dataset::cached(&cache, kind, rows, write)?));
    }
    if reference {
        let dir = Path::new("observations/reference");
        let rows = dataset::REFERENCE_ROWS;
        files.push((
            "parquet",
            rows,
            dataset::cached(dir, "parquet", rows, dataset::write_parquet)?,
        ));
    }
    let mut lines = String::new();
    for (kind, rows, path) in files {
        let path = std::path::absolute(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let label = dataset::label(rows);
        lines += &format!("import/{kind}_{label}\t{}\n", path.display());
    }
    Ok(lines)
}

/// `<tag><name> <value>` lines.
fn parse_bench(out: &str, tag: &str) -> Vec<(String, f64)> {
    out.lines()
        .filter_map(|l| l.strip_prefix(tag))
        .filter_map(|l| l.rsplit_once(' '))
        .filter_map(|(name, ns)| Some((name.to_owned(), ns.parse::<f64>().ok()?)))
        .collect()
}

/// `METRICCRAFT_BENCH_MACHINE`, else the hostname.
fn machine_id() -> String {
    std::env::var("METRICCRAFT_BENCH_MACHINE")
        .ok()
        .or_else(|| std::fs::read_to_string("/proc/sys/kernel/hostname").ok())
        .or_else(|| std::env::var("COMPUTERNAME").ok())
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".into())
}

fn machine_info(id: &str) -> serde_json::Value {
    let proc_field = |file: &str, key: &str| {
        std::fs::read_to_string(file).ok().and_then(|s| {
            s.lines()
                .find(|l| l.starts_with(key))
                .and_then(|l| l.split_once(':'))
                .map(|(_, v)| v.trim().to_owned())
        })
    };
    serde_json::json!({
        "id": id,
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "cpu": proc_field("/proc/cpuinfo", "model name"),
        "threads": std::thread::available_parallelism().map(|n| n.get()).ok(),
        "memory": proc_field("/proc/meminfo", "MemTotal"),
    })
}

// ---- parity ----

/// Scoreboard of parity Checks per covered tag (ADR 0004). `--update` rewrites
/// `parity/passing.txt` after an intended change; the gate itself is the `metriccraft-model` test.
fn parity(update: bool, pending: bool) -> Result {
    let root = Path::new("parity");
    let dax = metriccraft_model::parity::run(root)?;
    // Visual cases (M4.6) ratchet in their own list.
    let visuals = metriccraft_visuals::parity::run(&root.join("visuals"))?;
    let all: Vec<&metriccraft_model::parity::Check> =
        dax.checks.iter().chain(&visuals.checks).collect();
    if pending {
        for c in all.iter().filter(|c| !c.pass) {
            eprintln!("PENDING {}: {}", c.key(), c.detail);
        }
    }
    let mut tags = dax.by_tag();
    for (tag, (p, t)) in visuals.by_tag() {
        let e = tags.entry(tag).or_default();
        e.0 += p;
        e.1 += t;
    }
    for (tag, (pass, total)) in &tags {
        eprintln!("  {tag:<10} {pass:>3} / {total:<3} Checks");
    }
    let passing = all.iter().filter(|c| c.pass).count();
    let done = tags.values().filter(|(p, t)| p == t).count();
    eprintln!(
        "{passing} of {} Checks passing; {done} of {} tags complete",
        all.len(),
        tags.len()
    );
    if update {
        std::fs::write(root.join("passing.txt"), dax.passing_list())
            .map_err(|e| format!("writing parity/passing.txt: {e}"))?;
        std::fs::write(root.join("visuals/passing.txt"), visuals.passing_list())
            .map_err(|e| format!("writing parity/visuals/passing.txt: {e}"))?;
        eprintln!("wrote parity/passing.txt and parity/visuals/passing.txt");
        return Ok(());
    }
    let mut problems = dax.problems();
    problems.extend(visuals.problems());
    report(problems)
}

/// Writes one folder per case into the gitignored `observations/cases/` (cleared first), ready
/// to copy to `C:\metriccraft-parity\` in the Windows VM: query, datasets, CSV files byte-exact,
/// M with the VM path, and a README (`metriccraft_model::parity::export`, docs/parity.md).
fn parity_export() -> Result {
    let out = Path::new("observations/cases");
    if out.exists() {
        std::fs::remove_dir_all(out).map_err(|e| format!("clearing {}: {e}", out.display()))?;
    }
    let mut cases = metriccraft_model::parity::cases(Path::new("parity"))?;
    cases.extend(metriccraft_model::parity::cases_in(Path::new(
        "parity/visuals",
    ))?);
    for case in cases {
        for (name, body) in metriccraft_model::parity::export(&case)? {
            let path = out.join(&name);
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)
                    .map_err(|e| format!("creating {}: {e}", dir.display()))?;
            }
            std::fs::write(&path, body).map_err(|e| format!("writing {}: {e}", path.display()))?;
        }
        eprintln!("  {}", out.join(&case.id).display());
    }
    eprintln!(
        "copy {} to {} in the VM",
        out.display(),
        metriccraft_model::parity::VM_DIR
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(name: &str, deps: &[&str]) -> Krate {
        Krate {
            name: name.into(),
            deps: deps.iter().map(|d| (*d).into()).collect(),
        }
    }

    #[test]
    fn layers_allow_down_and_same_layer() {
        let ks = [
            k(
                "metriccraft-model",
                &["metriccraft-data", "metriccraft-core"],
            ),
            k("metriccraft-engine", &["metriccraft-model", "serde"]),
        ];
        assert!(check_layers(&ks).is_empty());
    }

    #[test]
    fn layers_reject_upward_dependency() {
        let errs = check_layers(&[k("metriccraft-core", &["metriccraft-engine"])]);
        assert_eq!(errs.len(), 1, "{errs:?}");
    }

    #[test]
    fn layers_reject_ui_crate_below_ui_layer() {
        assert_eq!(check_layers(&[k("metriccraft-engine", &["egui"])]).len(), 1);
        assert!(check_layers(&[k("metriccraft-ui-egui", &["egui"])]).is_empty());
    }

    #[test]
    fn layers_reject_unknown_crate() {
        assert_eq!(check_layers(&[k("metriccraft-new", &[])]).len(), 1);
    }

    const ATTR: &str = "| Asset | Author |\n|-------|--------|\n| `assets/logo.svg` | Martin |\n";

    #[test]
    fn widget_text_must_come_from_the_catalogue() {
        let src = "ui.label(Str::Ready.text());\nui.button(\"Save\");\nui.checkbox(&mut on, \"\");\n\
                   ui.add(egui::Button::new(\"+\"));\nr.on_hover_text(\"x\")\n// ui.label(\"ok\")\n\
                   #[cfg(test)]\nui.label(\"test\");";
        assert_eq!(literal_widget_text(src), [2, 5]);
    }

    #[test]
    fn assets_require_a_row() {
        let files = ["assets/logo.svg", "assets/icon.png", "src/main.rs"];
        let errs = check_assets(files.into_iter(), ATTR, |_| true);
        assert_eq!(
            errs,
            ["assets/icon.png: asset without an ATTRIBUTION.md row"]
        );
    }

    #[test]
    fn assets_reject_rows_for_missing_files() {
        let errs = check_assets(std::iter::empty(), ATTR, |_| false);
        assert_eq!(errs.len(), 1, "{errs:?}");
    }

    #[test]
    fn bench_lines_parse() {
        let out = "running 0 tests\nBENCH engine/x 12.5\nBENCH bad\nBENCH a b 3\nBENCHMEM m 7\n";
        let got = parse_bench(out, "BENCH ");
        assert_eq!(got.len(), 2, "{got:?}");
        assert_eq!(got[0].0, "engine/x");
        assert_eq!(got[1].0, "a b");
        assert_eq!(parse_bench(out, "BENCHMEM "), [("m".to_owned(), 7.0)]);
    }

    #[test]
    fn own_text_data_is_not_an_asset() {
        assert!(!is_asset("crates/dax/tests/parity/sum.json"));
        assert!(is_asset("assets/Font.TTF"));
    }
}
