//! Repo automation: `cargo xtask <task>`.
//!
//! Tasks: `ci` (everything below, in order), `fmt`, `clippy`, `test`, `layers`, `assets`, `wasm`,
//! `bench [--save]`, `parity [--update | --export]`.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::{Command, ExitCode};

type Result<T = ()> = std::result::Result<T, String>;

/// Layer of every workspace crate (`docs/plan/0001-mvp.md` §1). A crate may depend only on
/// crates in the same or a lower layer. A crate missing here fails `layers`.
const LAYERS: &[(&str, u8)] = &[
    ("opendash-core", 0),
    ("opendash-dax", 1),
    ("opendash-m", 1),
    ("opendash-data", 2),
    ("opendash-model", 2),
    ("opendash-file", 2),
    ("opendash-visuals", 3),
    ("opendash-engine", 5),
    ("opendash-ui-egui", 6),
    ("opendash-mcp", 6),
    ("opendash", 7),
    ("opendash-cli", 7),
    ("opendash-web", 7),
    ("xtask", 8),
];

/// Crates that only the UI layer (6) and above may depend on: the UI crate is swappable.
const UI_ONLY: &[&str] = &["egui", "eframe", "winit", "rfd"];

/// Native-only binaries left out of the wasm build.
const NOT_WASM: &[&str] = &["opendash", "opendash-cli", "xtask"];

/// File extensions that count as assets and need an `ATTRIBUTION.md` row. Text data we write
/// ourselves (JSON, CSV, parity values) is not an asset; binary media, fonts and documents are.
const ASSET_EXTS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "webp", "bmp", "ico", "icns", "svg", "ttf", "otf", "woff",
    "woff2", "wav", "mp3", "ogg", "xlsx", "parquet", "pbix", "pbit", "odash",
];

fn main() -> ExitCode {
    let task = std::env::args().nth(1).unwrap_or_default();
    let result = match task.as_str() {
        "ci" => ci(),
        "fmt" => fmt(),
        "clippy" => clippy(),
        "test" => test(),
        "layers" => layers(),
        "assets" => assets(),
        "wasm" => wasm(),
        "bench" => bench(std::env::args().any(|a| a == "--save")),
        "parity" if std::env::args().any(|a| a == "--export") => parity_export(),
        "parity" => parity(std::env::args().any(|a| a == "--update")),
        _ => {
            Err("usage: cargo xtask <ci|fmt|clippy|test|layers|assets|wasm|bench [--save]>".into())
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
    cargo(&["test", "--workspace"])
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

/// Runs every `harness = false` bench, compares with `bench/<machine>.json` and, with `--save`,
/// records the results as that machine's new baseline. Machines without a baseline only report.
fn bench(save: bool) -> Result {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let out = output(&cargo, &["bench", "--workspace", "--benches", "--quiet"])?;
    let results = parse_bench(&out);
    if results.is_empty() {
        return Err("no BENCH lines in `cargo bench` output".into());
    }
    let machine = machine_id();
    let path = format!("bench/{machine}.json");
    let baseline = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok());
    let base = |name: &str| baseline.as_ref().and_then(|b| b["results"][name].as_f64());
    let mut errors = Vec::new();
    for (name, ns) in &results {
        match base(name) {
            Some(was) => {
                let ratio = ns / was;
                eprintln!("  {name}: {ns:.1} ns (baseline {was:.1}, x{ratio:.2})");
                if ratio > BENCH_TOLERANCE && !save {
                    errors.push(format!(
                        "{name}: x{ratio:.2} slower than the {machine} baseline"
                    ));
                }
            }
            None => eprintln!("  {name}: {ns:.1} ns (no {machine} baseline)"),
        }
    }
    if save {
        let json = serde_json::json!({
            "machine": machine_info(&machine),
            "results": results
                .iter()
                .map(|(n, ns)| (n.clone(), serde_json::json!(ns)))
                .collect::<serde_json::Map<_, _>>(),
        });
        let text = serde_json::to_string_pretty(&json).map_err(|e| e.to_string())?;
        std::fs::create_dir_all("bench").map_err(|e| format!("creating bench/: {e}"))?;
        std::fs::write(&path, text + "\n").map_err(|e| format!("writing {path}: {e}"))?;
        eprintln!("  saved {path}");
    }
    report(errors)
}

fn parse_bench(out: &str) -> Vec<(String, f64)> {
    out.lines()
        .filter_map(|l| l.strip_prefix("BENCH "))
        .filter_map(|l| l.rsplit_once(' '))
        .filter_map(|(name, ns)| Some((name.to_owned(), ns.parse::<f64>().ok()?)))
        .collect()
}

/// `OPENDASH_BENCH_MACHINE`, else the hostname.
fn machine_id() -> String {
    std::env::var("OPENDASH_BENCH_MACHINE")
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
/// `parity/passing.txt` after an intended change; the gate itself is the `opendash-model` test.
fn parity(update: bool) -> Result {
    let root = Path::new("parity");
    let run = opendash_model::parity::run(root)?;
    let tags = run.by_tag();
    for (tag, (pass, total)) in &tags {
        eprintln!("  {tag:<10} {pass:>3} / {total:<3} Checks");
    }
    let passing = run.checks.iter().filter(|c| c.pass).count();
    let done = tags.values().filter(|(p, t)| p == t).count();
    eprintln!(
        "{passing} of {} Checks passing; {done} of {} tags complete",
        run.checks.len(),
        tags.len()
    );
    if update {
        std::fs::write(root.join("passing.txt"), run.passing_list())
            .map_err(|e| format!("writing parity/passing.txt: {e}"))?;
        eprintln!("wrote parity/passing.txt");
        return Ok(());
    }
    report(run.problems())
}

/// Writes each case's datasets and query as plain CRLF files into the gitignored
/// `observations/cases/`, ready to copy from Notepad in the Windows VM.
fn parity_export() -> Result {
    let out = Path::new("observations/cases");
    std::fs::create_dir_all(out).map_err(|e| format!("creating {}: {e}", out.display()))?;
    let crlf = |lines: Vec<String>| (lines.join("\n") + "\n").replace('\n', "\r\n");
    for case in opendash_model::parity::cases(Path::new("parity"))? {
        let mut files = vec![(format!("{}.dax", case.id), crlf(vec![case.query.clone()]))];
        for t in &case.tables {
            let mut lines = vec![t.columns.join("\t")];
            lines.extend(t.rows.iter().map(|r| r.join("\t")));
            files.push((format!("{}.{}.tsv", case.id, t.name), crlf(lines)));
        }
        for (name, body) in files {
            let path = out.join(&name);
            std::fs::write(&path, body).map_err(|e| format!("writing {}: {e}", path.display()))?;
            eprintln!("  {}", path.display());
        }
    }
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
            k("opendash-model", &["opendash-data", "opendash-core"]),
            k("opendash-engine", &["opendash-model", "serde"]),
        ];
        assert!(check_layers(&ks).is_empty());
    }

    #[test]
    fn layers_reject_upward_dependency() {
        let errs = check_layers(&[k("opendash-core", &["opendash-engine"])]);
        assert_eq!(errs.len(), 1, "{errs:?}");
    }

    #[test]
    fn layers_reject_ui_crate_below_ui_layer() {
        assert_eq!(check_layers(&[k("opendash-engine", &["egui"])]).len(), 1);
        assert!(check_layers(&[k("opendash-ui-egui", &["egui"])]).is_empty());
    }

    #[test]
    fn layers_reject_unknown_crate() {
        assert_eq!(check_layers(&[k("opendash-new", &[])]).len(), 1);
    }

    const ATTR: &str = "| Asset | Author |\n|-------|--------|\n| `assets/logo.svg` | Martin |\n";

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
        let out = "running 0 tests\nBENCH engine/x 12.5\nBENCH bad\nBENCH a b 3\n";
        let got = parse_bench(out);
        assert_eq!(got.len(), 2, "{got:?}");
        assert_eq!(got[0].0, "engine/x");
        assert_eq!(got[1].0, "a b");
    }

    #[test]
    fn own_text_data_is_not_an_asset() {
        assert!(!is_asset("crates/dax/tests/parity/sum.json"));
        assert!(is_asset("assets/Font.TTF"));
    }
}
