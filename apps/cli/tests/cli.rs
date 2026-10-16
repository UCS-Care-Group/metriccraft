//! Every verb of `metriccraft-cli`, run as a process on a Project built in code.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use metriccraft_engine::App;
use serde_json::{Value, json};

/// A Project folder holding `Sales` (from a CSV beside it), a Measure and a Page with a card;
/// removed on drop.
struct Fixture {
    dir: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[cfg(test)]
impl Fixture {
    fn new(name: &str) -> Fixture {
        let dir = std::env::temp_dir().join(format!("mcraft-cli-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let csv = dir.join("Sales.csv");
        std::fs::write(&csv, "Region,Qty\nNorth,3\nSouth,4\nNorth,5\n").unwrap();
        let mut app = App::new();
        let run = |app: &mut App, id: &str, p: Value| app.run(id, &p).unwrap();
        run(
            &mut app,
            "home.getData",
            json!({"kind": "csv", "path": csv.display().to_string()}),
        );
        run(
            &mut app,
            "modeling.newMeasure",
            json!({"table": "Sales", "name": "Total", "expression": "SUM(Sales[Qty])"}),
        );
        run(&mut app, "insert.newVisual", json!({"type": "card"}));
        let fx = Fixture { dir };
        run(
            &mut app,
            "file.saveAs",
            json!({"path": fx.project().display().to_string()}),
        );
        fx
    }

    fn project(&self) -> PathBuf {
        self.dir.join("project")
    }

    fn cli(&self, args: &[&str]) -> Output {
        cli_in(&self.project(), args, None)
    }
}

#[cfg(test)]
fn cli_in(project: &Path, args: &[&str], stdin: Option<&str>) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_metriccraft-cli"));
    if let Some((verb, rest)) = args.split_first() {
        cmd.arg(verb);
        if *verb != "commands" {
            cmd.arg(project);
        }
        cmd.args(rest);
    }
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().unwrap();
    if let Some(text) = stdin {
        use std::io::Write;
        child
            .stdin
            .take()
            .unwrap()
            .write_all(text.as_bytes())
            .unwrap();
    }
    drop(child.stdin.take());
    child.wait_with_output().unwrap()
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn code(o: &Output) -> Option<i32> {
    o.status.code()
}

#[cfg(test)]
fn describe(fx: &Fixture) -> Value {
    let o = fx.cli(&["describe"]);
    assert_eq!(code(&o), Some(0), "{}", stderr(&o));
    serde_json::from_slice(&o.stdout).unwrap()
}

#[test]
fn describe_prints_the_project_as_json() {
    let fx = Fixture::new("describe");
    let d = describe(&fx);
    assert_eq!(d["tables"][0]["name"], "Sales");
    assert_eq!(d["tables"][0]["rows"], 3);
    assert_eq!(d["tables"][0]["measures"][0]["name"], "Total");
    assert_eq!(d["pages"][0]["visuals"][0]["type"], "card");
}

#[test]
fn dax_prints_csv_when_piped_and_json_or_table_on_request() {
    let fx = Fixture::new("dax");
    let q = "EVALUATE SUMMARIZECOLUMNS(Sales[Region], \"T\", [Total]) ORDER BY Sales[Region]";
    let o = fx.cli(&["dax", q]);
    assert_eq!(code(&o), Some(0), "{}", stderr(&o));
    assert_eq!(stdout(&o), "Region,T\nNorth,8\nSouth,4\n");

    let o = fx.cli(&["dax", q, "--format", "table"]);
    assert_eq!(stdout(&o), "Region  T\n------  -\nNorth   8\nSouth   4\n");

    let o = fx.cli(&["dax", q, "--format=json", "--max-rows", "1"]);
    let v: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["rows"], json!([["North", 8]]));
    assert_eq!(v["truncated"], true);
    assert!(stderr(&o).contains("first 1 rows shown"), "{}", stderr(&o));
}

#[test]
fn render_writes_a_png() {
    let fx = Fixture::new("render");
    let out = fx.dir.join("page.png");
    let o = fx.cli(&[
        "render",
        "--out",
        &out.display().to_string(),
        "--scale",
        "0.5",
    ]);
    assert_eq!(code(&o), Some(0), "{}", stderr(&o));
    let png = std::fs::read(&out).unwrap();
    assert_eq!(png.get(..8), Some(&b"\x89PNG\r\n\x1a\n"[..]));
}

#[test]
fn run_writes_nothing_without_save_and_saves_with_it() {
    let fx = Fixture::new("run");
    let params = r#"{"table": "Sales", "name": "Avg", "expression": "AVERAGE(Sales[Qty])"}"#;
    let o = fx.cli(&["run", "modeling.newMeasure", "--params", params]);
    assert_eq!(code(&o), Some(0), "{}", stderr(&o));
    assert!(stderr(&o).contains("not saved"), "{}", stderr(&o));
    assert_eq!(
        describe(&fx)["tables"][0]["measures"]
            .as_array()
            .map(Vec::len),
        Some(1)
    );

    let o = fx.cli(&["run", "modeling.newMeasure", "--params", params, "--save"]);
    assert_eq!(code(&o), Some(0), "{}", stderr(&o));
    let names = describe(&fx)["tables"][0]["measures"].to_string();
    assert!(names.contains("\"Avg\""), "{names}");
    let o = fx.cli(&["dax", "EVALUATE ROW(\"a\", [Avg])"]);
    assert_eq!(stdout(&o), "a\n4\n");
}

#[test]
fn run_prints_a_read_commands_result() {
    let fx = Fixture::new("run-read");
    let o = fx.cli(&["run", "document.describe"]);
    assert_eq!(code(&o), Some(0), "{}", stderr(&o));
    let v: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["tables"][0]["name"], "Sales");
}

#[test]
fn script_runs_lines_in_order_and_save_as_writes_elsewhere() {
    let fx = Fixture::new("script");
    let steps = fx.dir.join("steps.jsonl");
    std::fs::write(
        &steps,
        "{\"run\": \"modeling.renameMeasure\", \"params\": {\"table\": \"Sales\", \"measure\": \"Total\", \"name\": \"Qty Total\"}}\n\n\
         {\"run\": \"dax.evaluate\", \"params\": {\"query\": \"EVALUATE ROW(\\\"q\\\", [Qty Total])\"}}\n",
    )
    .unwrap();
    let copy = fx.dir.join("copy.mcraft");
    let o = fx.cli(&[
        "script",
        &steps.display().to_string(),
        "--save-as",
        &copy.display().to_string(),
    ]);
    assert_eq!(code(&o), Some(0), "{}", stderr(&o));
    let line: Value = serde_json::from_str(stdout(&o).trim()).unwrap();
    assert_eq!(line["rows"], json!([[12]]));
    assert!(copy.is_file());
    // The original is untouched.
    assert_eq!(describe(&fx)["tables"][0]["measures"][0]["name"], "Total");
    let o = cli_in(&copy, &["dax", "EVALUATE ROW(\"q\", [Qty Total])"], None);
    assert_eq!(stdout(&o), "q\n12\n", "{}", stderr(&o));
}

#[test]
fn script_reads_stdin_and_stops_at_the_failing_line() {
    let fx = Fixture::new("script-fail");
    let steps = "{\"run\": \"insert.newPage\"}\n{\"run\": \"no.such\"}\n";
    let o = cli_in(&fx.project(), &["script", "-", "--save"], Some(steps));
    assert_eq!(code(&o), Some(1));
    assert!(stderr(&o).contains("line 2"), "{}", stderr(&o));
    // Nothing saved: the Page from line 1 isn't there.
    assert_eq!(describe(&fx)["pages"].as_array().map(Vec::len), Some(1));
}

#[test]
fn refresh_reloads_and_reports_rows() {
    let fx = Fixture::new("refresh");
    std::fs::write(fx.dir.join("Sales.csv"), "Region,Qty\nEast,1\n").unwrap();
    let o = fx.cli(&["refresh"]);
    assert_eq!(code(&o), Some(0), "{}", stderr(&o));
    assert!(stderr(&o).contains("Sales: 1 rows"), "{}", stderr(&o));
    assert_eq!(describe(&fx)["tables"][0]["rows"], 3, "not saved");

    let o = fx.cli(&["refresh", "--save"]);
    assert_eq!(code(&o), Some(0), "{}", stderr(&o));
    assert_eq!(describe(&fx)["tables"][0]["rows"], 1);
}

#[test]
fn commands_lists_every_command() {
    let o = cli_in(Path::new(""), &["commands"], None);
    assert_eq!(code(&o), Some(0));
    assert!(stdout(&o).contains("dax.evaluate"));
    let o = cli_in(Path::new(""), &["commands", "--json"], None);
    let list: Value = serde_json::from_slice(&o.stdout).unwrap();
    let ids: Vec<&str> = list
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|c| c["id"].as_str())
        .collect();
    assert_eq!(ids.len(), metriccraft_engine::COMMANDS.len());
    let eval = list
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == "dax.evaluate")
        .unwrap();
    assert_eq!(eval["readOnly"], true);
    assert_eq!(eval["params"][0]["name"], "query");
}

#[test]
fn usage_errors_exit_2_and_command_errors_exit_1() {
    let fx = Fixture::new("exit");
    for args in [
        &["frobnicate"][..],
        &["dax"],
        &["dax", "EVALUATE Sales", "--format", "xml"],
        &["run", "file.save"],
        &["run", "insert.newPage", "--params", "{"],
        &["run", "insert.newPage", "--save", "--save-as", "x"],
        &["render"],
        &["describe", "--bogus"],
    ] {
        let o = fx.cli(args);
        assert_eq!(code(&o), Some(2), "{args:?}: {}", stderr(&o));
    }
    let o = cli_in(Path::new(""), &[], None);
    assert_eq!(code(&o), Some(2));
    for args in [&["dax", "EVALUATE Nope"][..], &["run", "no.such"]] {
        let o = fx.cli(args);
        assert_eq!(code(&o), Some(1), "{args:?}: {}", stderr(&o));
    }
    let o = cli_in(&fx.dir.join("missing"), &["describe"], None);
    assert_eq!(code(&o), Some(1), "{}", stderr(&o));
}
