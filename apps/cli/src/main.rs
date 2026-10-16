//! MetricCraft headless CLI (no GUI libraries): opens a Project folder or `.mcraft` Package,
//! runs commands through the engine as Origin CLI, and writes nothing without `--save` or
//! `--save-as`. Exit 0 on success, 1 when a command fails, 2 on a usage error.

use std::collections::BTreeMap;
use std::io::{IsTerminal, Write};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use metriccraft_engine::{App, COMMANDS, Origin, Progress};
use serde_json::{Value, json};

const USAGE: &str = "\
Usage: metriccraft-cli <verb> [args]

  describe P                         what the Project holds, as JSON
  refresh P [SAVE]                   reload every Query from its Sources
  dax P QUERY [--format table|csv|json] [--max-rows N]
                                     run a DAX query (table on a terminal, CSV when piped)
  render P --out FILE.png [--page PAGE] [--scale S | --width PX]
                                     draw a Page as PNG
  run P ID [--params JSON] [SAVE]    run one command
  script P FILE [SAVE]               run JSON lines {\"run\": id, \"params\": {...}}; FILE - is stdin
  run|script --new ... --save-as PATH
                                     the same on a new, empty Project instead of P
  commands [--json]                  every command and its params

P is a Project folder, its project.mcraft.json, or a .mcraft Package.
SAVE is --save (where P is) or --save-as PATH; without one nothing is written.
Ctrl+C cancels a running load or query. Exit 0 ok, 1 command failed, 2 usage.";

/// Why the CLI stopped: a usage error (exit 2) or a failed command (exit 1).
enum Fail {
    Usage(String),
    Command(String),
}

impl From<metriccraft_engine::Error> for Fail {
    fn from(e: metriccraft_engine::Error) -> Self {
        Fail::Command(e.to_string())
    }
}

type Result<T = ()> = std::result::Result<T, Fail>;

fn usage<T>(message: impl Into<String>) -> Result<T> {
    Err(Fail::Usage(message.into()))
}

/// Set by the first Ctrl+C; the second one quits at once.
static CANCELLED: AtomicBool = AtomicBool::new(false);

/// A line on stderr; a broken stderr isn't worth failing over.
macro_rules! note {
    ($($t:tt)*) => {{
        let _ = writeln!(std::io::stderr(), $($t)*);
    }};
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match cli(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(Fail::Usage(m)) => {
            note!("error: {m}\n(metriccraft-cli --help for usage)");
            ExitCode::from(2)
        }
        Err(Fail::Command(m)) => {
            note!("error: {m}");
            ExitCode::from(1)
        }
    }
}

fn cli(args: &[String]) -> Result {
    let (verb, rest) = match args.split_first() {
        Some((v, rest)) => (v.as_str(), rest),
        None => return usage("no verb given"),
    };
    match verb {
        "-h" | "--help" | "help" => return print(USAGE),
        "-V" | "--version" => {
            return print(&format!("metriccraft-cli {}", env!("CARGO_PKG_VERSION")));
        }
        "commands" => {
            let a = Args::parse(rest, &[], &["--json"])?;
            a.positionals(0)?;
            return commands(a.has("--json"));
        }
        _ => {}
    }
    let (takes, switches): (&[&str], &[&str]) = match verb {
        "describe" => (&[], &[]),
        "refresh" => (&["--save-as"], &["--save"]),
        "dax" => (&["--format", "--max-rows"], &[]),
        "render" => (&["--out", "--page", "--scale", "--width"], &[]),
        "run" => (&["--params", "--save-as"], &["--save", "--new"]),
        "script" => (&["--save-as"], &["--save", "--new"]),
        _ => return usage(format!("unknown verb `{verb}`")),
    };
    let a = Args::parse(rest, takes, switches)?;
    // `--new` starts from an empty Project instead of opening P.
    let new = a.has("--new");
    let wants = match verb {
        "describe" | "refresh" | "render" => 1,
        _ if new => 1,
        _ => 2,
    };
    let pos = a.positionals(wants)?;
    let save = a.save()?;
    if new && save == Some(None) {
        return usage("a --new Project has nowhere to --save; use --save-as PATH");
    }
    let (project, arg) = match pos.as_slice() {
        [x] if new => (None, x.as_str()),
        [p] => (Some(p.as_str()), ""),
        [p, x] => (Some(p.as_str()), x.as_str()),
        _ => return usage("wrong number of arguments"),
    };
    // Checked before opening, so a usage error never costs a load.
    let format = match a.get("--format") {
        None if std::io::stdout().is_terminal() => "table",
        None => "csv",
        Some(f @ ("table" | "csv" | "json")) => f,
        Some(f) => return usage(format!("--format must be table, csv or json, not `{f}`")),
    };
    let params = match a.get("--params") {
        None => Value::Null,
        Some(p) => serde_json::from_str(p)
            .map_err(|e| Fail::Usage(format!("--params is not JSON: {e}")))?,
    };
    if verb == "run" {
        writes_nothing(arg)?;
    }
    if verb == "render" && a.get("--out").is_none() {
        return usage("render needs --out FILE.png");
    }
    let script = if verb == "script" {
        let text = if arg == "-" {
            std::io::read_to_string(std::io::stdin())
        } else {
            std::fs::read_to_string(arg)
        };
        Some(text.map_err(|e| Fail::Command(format!("reading {arg}: {e}")))?)
    } else {
        None
    };

    let mut app = App::new();
    on_ctrl_c(&app);
    if let Some(project) = project {
        run(&mut app, "file.open", &json!({"path": project}))?;
    }
    let out = match verb {
        "describe" => pretty(&run(&mut app, "document.describe", &Value::Null)?),
        "refresh" => {
            let watch = Watch::start(app.load_progress());
            let done = run(&mut app, "home.refresh", &Value::Null);
            drop(watch);
            done?;
            for t in app
                .last_load()
                .map(|r| r.tables.as_slice())
                .unwrap_or_default()
            {
                let errors = match t.cell_errors {
                    0 => String::new(),
                    n => format!(" ({n} cell errors, loaded as blank)"),
                };
                note!("{}: {} rows{errors}", t.table, t.rows);
            }
            Ok(())
        }
        "dax" => dax(&mut app, arg, a.get("--max-rows"), format),
        "render" => render(&mut app, &a),
        "run" => {
            let watch = Watch::start(app.load_progress());
            let done = run(&mut app, arg, &params);
            drop(watch);
            match done? {
                Value::Null => Ok(()),
                v => pretty(&v),
            }
        }
        _ => run_script(&mut app, script.as_deref().unwrap_or_default()),
    };
    out?;
    if CANCELLED.load(Ordering::Relaxed) {
        return Err(Fail::Command("cancelled".into()));
    }
    match save {
        Some(None) => run(&mut app, "file.save", &Value::Null).map(drop)?,
        Some(Some(path)) => run(&mut app, "file.saveAs", &json!({"path": path})).map(drop)?,
        None if app.is_dirty() => {
            note!("not saved: add --save or --save-as PATH to keep the changes")
        }
        None => {}
    }
    app.close()?;
    Ok(())
}

/// Runs `id` as the CLI; stops at a Ctrl+C that landed between commands.
fn run(app: &mut App, id: &str, params: &Value) -> Result<Value> {
    if CANCELLED.load(Ordering::Relaxed) {
        return Err(Fail::Command("cancelled".into()));
    }
    Ok(app.run_as(Origin::Cli, id, params)?)
}

/// `file.*` writes to disk (or replaces the Document) on its own: the CLI saves only through
/// `--save`/`--save-as`.
fn writes_nothing(id: &str) -> Result {
    if id.starts_with("file.") {
        return usage(format!(
            "`{id}` can't run from the CLI; use --save or --save-as"
        ));
    }
    Ok(())
}

/// The first Ctrl+C cancels the running load or query (it fails, changing nothing) and stops
/// what's left; the second quits at once.
fn on_ctrl_c(app: &App) {
    let progress = app.load_progress();
    let cancel = app.cancel_flag();
    let set = ctrlc::set_handler(move || {
        if CANCELLED.swap(true, Ordering::Relaxed) {
            std::process::exit(130);
        }
        progress.cancel();
        cancel.store(true, Ordering::Relaxed);
        note!("\ncancelling (Ctrl+C again to quit now)");
    });
    if let Err(e) = set {
        note!("Ctrl+C won't cancel: {e}");
    }
}

fn run_script(app: &mut App, text: &str) -> Result {
    let watch = Watch::start(app.load_progress());
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let at = |m: String| format!("line {}: {m}", n.saturating_add(1));
        let step: Value =
            serde_json::from_str(line).map_err(|e| Fail::Usage(at(format!("not JSON: {e}"))))?;
        let Some(id) = step.get("run").and_then(Value::as_str) else {
            return usage(at("needs {\"run\": command id, \"params\": {...}}".into()));
        };
        writes_nothing(id).map_err(|_| {
            Fail::Usage(at(format!(
                "`{id}` can't run from a script; use --save or --save-as"
            )))
        })?;
        let params = step.get("params").cloned().unwrap_or(Value::Null);
        match run(app, id, &params) {
            Ok(Value::Null) => {}
            Ok(v) => print(&v.to_string())?,
            Err(Fail::Command(m) | Fail::Usage(m)) => return Err(Fail::Command(at(m))),
        }
    }
    drop(watch);
    Ok(())
}

fn dax(app: &mut App, query: &str, max: Option<&str>, format: &str) -> Result {
    let mut params = json!({ "query": query });
    if let Some(m) = max {
        let n: i64 = m
            .parse()
            .map_err(|_| Fail::Usage(format!("--max-rows must be a whole number, not `{m}`")))?;
        params["maxRows"] = json!(n);
    }
    let out = run(app, "dax.evaluate", &params)?;
    let text = match format {
        "json" => serde_json::to_string_pretty(&out).unwrap_or_default(),
        "csv" => csv(&out)?,
        _ => table(&out),
    };
    print(text.trim_end())?;
    if out["truncated"] == true {
        let shown = out["rows"].as_array().map_or(0, Vec::len);
        note!("first {shown} rows shown; the result has more (--max-rows)");
    }
    Ok(())
}

fn render(app: &mut App, a: &Args) -> Result {
    let mut params = serde_json::Map::new();
    if let Some(p) = a.get("--page") {
        params.insert("page".into(), json!(p));
    }
    for (flag, name) in [("--scale", "scale"), ("--width", "width")] {
        if let Some(v) = a.get(flag) {
            let n: f64 = v
                .parse()
                .ok()
                .filter(|n: &f64| n.is_finite() && *n > 0.0)
                .ok_or_else(|| {
                    Fail::Usage(format!("{flag} must be a positive number, not `{v}`"))
                })?;
            params.insert(name.into(), json!(n));
        }
    }
    let out = run(app, "page.render", &Value::Object(params))?;
    let png = out["png"]
        .as_str()
        .and_then(unbase64)
        .ok_or_else(|| Fail::Command("page.render returned no image".into()))?;
    let path = a.get("--out").unwrap_or_default();
    std::fs::write(path, png).map_err(|e| Fail::Command(format!("writing {path}: {e}")))?;
    note!("wrote {path} ({}×{} px)", out["width"], out["height"]);
    Ok(())
}

fn commands(as_json: bool) -> Result {
    if as_json {
        let list: Vec<Value> = COMMANDS
            .iter()
            .map(|c| {
                json!({
                    "id": c.id,
                    "label": c.label,
                    "ribbon": c.ribbon,
                    "shortcut": c.shortcut,
                    "readOnly": c.read_only(),
                    "params": c.params.iter().map(|p| json!({
                        "name": p.name,
                        "kind": p.kind.word(),
                        "required": p.required,
                        "nullable": p.nullable,
                        "doc": p.doc,
                    })).collect::<Vec<_>>(),
                })
            })
            .collect();
        return pretty(&Value::Array(list));
    }
    let width = COMMANDS.iter().map(|c| c.id.len()).max().unwrap_or(0);
    let mut out = String::new();
    for c in COMMANDS {
        let read = if c.read_only() { " (read)" } else { "" };
        out.push_str(&format!("{:width$}  {}{read}\n", c.id, c.label));
        for line in metriccraft_engine::param::doc(c.params).lines() {
            out.push_str(&format!("{:width$}    {line}\n", ""));
        }
    }
    print(out.trim_end())
}

/// Writes `text` and a newline to stdout. A closed pipe (`| head`) ends quietly.
fn print(text: &str) -> Result {
    let mut out = std::io::stdout().lock();
    match writeln!(out, "{text}").and_then(|()| out.flush()) {
        Err(e) if e.kind() != std::io::ErrorKind::BrokenPipe => {
            Err(Fail::Command(format!("writing output: {e}")))
        }
        _ => Ok(()),
    }
}

fn pretty(v: &Value) -> Result {
    print(&serde_json::to_string_pretty(v).unwrap_or_default())
}

/// A `dax.evaluate` cell as text: BLANK is empty, a whole decimal has no `.0`.
fn text(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Number(n) if n.is_f64() => n.as_f64().map(|f| f.to_string()).unwrap_or_default(),
        v => v.to_string(),
    }
}

fn names(out: &Value) -> Vec<String> {
    out["columns"]
        .as_array()
        .map(|cs| cs.iter().map(|c| text(&c["name"])).collect())
        .unwrap_or_default()
}

fn rows(out: &Value) -> &[Value] {
    out["rows"].as_array().map_or(&[], Vec::as_slice)
}

fn csv(out: &Value) -> Result<String> {
    let mut w = csv::Writer::from_writer(Vec::new());
    let mut write = |cells: Vec<String>| {
        w.write_record(cells)
            .map_err(|e| Fail::Command(format!("writing CSV: {e}")))
    };
    write(names(out))?;
    for row in rows(out) {
        write(
            row.as_array()
                .map_or(&[][..], Vec::as_slice)
                .iter()
                .map(text)
                .collect(),
        )?;
    }
    let bytes = w
        .into_inner()
        .map_err(|e| Fail::Command(format!("writing CSV: {e}")))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Columns padded to their widest cell; numbers right-aligned.
fn table(out: &Value) -> String {
    let header = names(out);
    let body: Vec<Vec<(String, bool)>> = rows(out)
        .iter()
        .map(|r| {
            r.as_array()
                .map_or(&[][..], Vec::as_slice)
                .iter()
                .map(|v| (text(v), v.is_number()))
                .collect()
        })
        .collect();
    let mut widths: Vec<usize> = header.iter().map(|h| h.chars().count()).collect();
    for row in &body {
        for (i, (cell, _)) in row.iter().enumerate() {
            if let Some(w) = widths.get_mut(i) {
                *w = (*w).max(cell.chars().count());
            }
        }
    }
    let line = |cells: Vec<(String, bool)>| {
        let mut s = String::new();
        for (i, (cell, right)) in cells.iter().enumerate() {
            let w = widths.get(i).copied().unwrap_or(0);
            let pad = " ".repeat(w.saturating_sub(cell.chars().count()));
            if i > 0 {
                s.push_str("  ");
            }
            if *right {
                s.push_str(&pad);
                s.push_str(cell);
            } else {
                s.push_str(cell);
                s.push_str(&pad);
            }
        }
        s.trim_end().to_string() + "\n"
    };
    let mut s = line(header.into_iter().map(|h| (h, false)).collect());
    s.push_str(&line(
        widths.iter().map(|w| ("-".repeat(*w), false)).collect(),
    ));
    for row in body {
        s.push_str(&line(row));
    }
    s
}

/// Decodes standard base64 (as `page.render` writes it); `None` if it isn't.
fn unbase64(s: &str) -> Option<Vec<u8>> {
    let s = s.trim_end_matches('=').as_bytes();
    let mut out = Vec::with_capacity(s.len() / 4 * 3 + 3);
    let (mut acc, mut bits) = (0u32, 0u32);
    for &c in s {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        };
        acc = (acc << 6 | u32::from(v)) & 0xFF_FFFF;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

/// Rows read per Query on stderr while a load runs (only on a terminal).
struct Watch {
    done: Arc<Mutex<bool>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Watch {
    fn start(progress: Arc<Progress>) -> Self {
        let done = Arc::new(Mutex::new(false));
        if !std::io::stderr().is_terminal() {
            return Watch { done, thread: None };
        }
        let stop = Arc::clone(&done);
        let thread = std::thread::spawn(move || {
            let mut last = String::new();
            while !*stop.lock().unwrap_or_else(PoisonError::into_inner) {
                let now = progress
                    .rows()
                    .iter()
                    .map(|(q, n)| format!("{q}: {n} rows"))
                    .collect::<Vec<_>>()
                    .join(", ");
                if now != last {
                    let _ = write!(std::io::stderr(), "\r{now}\x1b[K");
                    last = now;
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            if !last.is_empty() {
                let _ = write!(std::io::stderr(), "\r\x1b[K");
            }
        });
        Watch {
            done,
            thread: Some(thread),
        }
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        *self.done.lock().unwrap_or_else(PoisonError::into_inner) = true;
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Positional arguments and `--flag value` / `--flag=value` options.
struct Args {
    positional: Vec<String>,
    options: BTreeMap<String, String>,
    switches: Vec<String>,
}

impl Args {
    fn parse(args: &[String], takes: &[&str], switches: &[&str]) -> Result<Args> {
        let mut a = Args {
            positional: Vec::new(),
            options: BTreeMap::new(),
            switches: Vec::new(),
        };
        let mut it = args.iter();
        while let Some(arg) = it.next() {
            let (flag, inline) = match arg.split_once('=') {
                Some((f, v)) if f.starts_with("--") => (f, Some(v.to_string())),
                _ => (arg.as_str(), None),
            };
            if takes.contains(&flag) {
                let value = match inline {
                    Some(v) => v,
                    None => match it.next() {
                        Some(v) => v.clone(),
                        None => return usage(format!("{flag} needs a value")),
                    },
                };
                if a.options.insert(flag.to_string(), value).is_some() {
                    return usage(format!("{flag} given twice"));
                }
            } else if switches.contains(&flag) && inline.is_none() {
                a.switches.push(flag.to_string());
            } else if flag.starts_with("--") {
                return usage(format!("unknown option `{flag}`"));
            } else {
                a.positional.push(arg.clone());
            }
        }
        Ok(a)
    }

    fn positionals(&self, n: usize) -> Result<&Vec<String>> {
        if self.positional.len() != n {
            return usage(format!(
                "expected {n} argument(s), got {}",
                self.positional.len()
            ));
        }
        Ok(&self.positional)
    }

    fn get(&self, flag: &str) -> Option<&str> {
        self.options.get(flag).map(String::as_str)
    }

    fn has(&self, flag: &str) -> bool {
        self.switches.iter().any(|s| s == flag)
    }

    /// `--save` → `Some(None)`, `--save-as P` → `Some(Some(P))`.
    fn save(&self) -> Result<Option<Option<&str>>> {
        match (self.has("--save"), self.get("--save-as")) {
            (true, Some(_)) => usage("give --save or --save-as, not both"),
            (true, None) => Ok(Some(None)),
            (false, Some(p)) => Ok(Some(Some(p))),
            (false, None) => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unbase64_decodes_standard_base64() {
        for (s, bytes) in [
            ("", &b""[..]),
            ("Zg==", b"f"),
            ("Zm8=", b"fo"),
            ("Zm9v", b"foo"),
            ("Zm9vYg==", b"foob"),
            ("AP+ABwk=", &[0, 255, 128, 7, 9]),
        ] {
            assert_eq!(unbase64(s).as_deref(), Some(bytes), "{s}");
        }
        assert_eq!(unbase64("a$b"), None);
    }

    #[test]
    fn table_pads_columns_and_right_aligns_numbers() {
        let out = json!({
            "columns": [{"name": "Region"}, {"name": "Total"}],
            "rows": [["North", 8], ["South", null], ["Ea", 1234.0]],
        });
        assert_eq!(
            table(&out),
            "Region  Total\n------  -----\nNorth       8\nSouth\nEa       1234\n"
        );
    }

    #[test]
    fn csv_quotes_and_leaves_blank_empty() {
        let out = json!({"columns": [{"name": "a,b"}], "rows": [["x\"y"], [null]]});
        assert_eq!(
            csv(&out).ok().as_deref(),
            Some("\"a,b\"\n\"x\"\"y\"\n\"\"\n")
        );
    }
}
