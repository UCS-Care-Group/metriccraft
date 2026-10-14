//! Headless screenshots of the window (wgpu, offscreen): what the UI looks like without opening
//! a window on anyone's screen.
//!
//! `cargo run -p opendash-ui-egui --example ui_shot -- OUT.png [SCRIPT.jsonl]`
//!
//! The script is JSON lines, run in order: `{"run": "<command>", "params": {…}}` runs a command
//! and waits for it; `{"dark": true}` switches to the dark App theme; `{"view": "table"}` picks a
//! View; `{"select": "$1"}` selects a Visual on the canvas; `{"invoke": "<command>"}` invokes a
//! command as its Ribbon button does; `{"type": "text"}` types into the focused field; `{"shot": "/abs/out.png"}` saves a
//! PNG there too. A string `"$N"` anywhere stands for the id of the Nth Visual the script
//! created (ids are random). The window is 1440×900 pt at 2×.

use std::time::{Duration, Instant};

use opendash_engine::App;
use opendash_ui_egui::worker::Engine;
use opendash_ui_egui::{Prefs, Shell};
use serde_json::Value;

type Harness<'a> = egui_kittest::Harness<'a, Shell>;

fn settle(h: &mut Harness<'_>, until_seq: Option<u64>) {
    let end = Instant::now() + Duration::from_secs(30);
    loop {
        h.step();
        let done = until_seq.is_none_or(|s| h.state().snapshot().seq >= s);
        if (done && h.state().snapshot().busy.is_none()) || Instant::now() > end {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    for _ in 0..4 {
        h.step();
    }
}

fn shot(h: &mut Harness<'_>, path: &str) -> Result<(), String> {
    let img = h.render().map_err(|e| format!("render: {e}"))?;
    img.save(path).map_err(|e| format!("save {path}: {e}"))?;
    println!("{path}");
    Ok(())
}

/// `v` with each `"$N"` string replaced by the Nth created Visual's id.
fn ids(seen: &[String], v: Value) -> Value {
    match v {
        Value::String(s) if s.starts_with('$') => {
            let n: usize = s[1..].parse().unwrap_or(0);
            seen.get(n.wrapping_sub(1))
                .cloned()
                .map_or(Value::String(s), Value::String)
        }
        Value::Array(a) => Value::Array(a.into_iter().map(|x| ids(seen, x)).collect()),
        Value::Object(o) => Value::Object(o.into_iter().map(|(k, x)| (k, ids(seen, x))).collect()),
        v => v,
    }
}

/// Records Visuals not seen before, in creation order.
fn track(h: &Harness<'_>, seen: &mut Vec<String>) {
    for p in &h.state().snapshot().doc.report.pages {
        for v in &p.visuals {
            if !seen.contains(&v.id) {
                seen.push(v.id.clone());
            }
        }
    }
}

fn main() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let out = args.next().ok_or("usage: ui_shot OUT.png [SCRIPT.jsonl]")?;
    let script = match args.next() {
        Some(p) => std::fs::read_to_string(&p).map_err(|e| format!("{p}: {e}"))?,
        None => String::new(),
    };
    let engine = Engine::new(App::new(), || {});
    let shell = Shell::new(engine, Prefs::default(), (None, Vec::new()));
    let mut h = egui_kittest::Harness::builder()
        .with_size(egui::vec2(1440.0, 900.0))
        .with_pixels_per_point(2.0)
        .wgpu()
        .build_ui_state(
            |ui, shell: &mut Shell| {
                shell.logic(ui.ctx());
                shell.ui(ui);
            },
            shell,
        );
    h.state().setup(&h.ctx);
    settle(&mut h, None);
    let mut seen = Vec::new();
    for line in script.lines().filter(|l| !l.trim().is_empty()) {
        let l: Value = serde_json::from_str(line).map_err(|e| format!("{line}: {e}"))?;
        let l = ids(&seen, l);
        if let Some(id) = l.get("run").and_then(Value::as_str) {
            let seq = h.state().snapshot().seq + 1;
            let params = l.get("params").cloned().unwrap_or(Value::Null);
            h.state_mut().run(id, params);
            settle(&mut h, Some(seq));
            track(&h, &mut seen);
        } else if l.get("dark").and_then(Value::as_bool) == Some(true) {
            let ctx = h.ctx.clone();
            h.state_mut().set_dark(&ctx);
            settle(&mut h, None);
        } else if let Some(v) = l.get("view").and_then(Value::as_str) {
            h.state_mut().set_view(v);
            settle(&mut h, None);
        } else if let Some(id) = l.get("invoke").and_then(Value::as_str) {
            h.state_mut().invoke(id);
            settle(&mut h, None);
        } else if let Some(text) = l.get("type").and_then(Value::as_str) {
            h.input_mut()
                .events
                .push(egui::Event::Text(text.to_string()));
            settle(&mut h, None);
        } else if let Some(v) = l.get("select").and_then(Value::as_str) {
            h.state_mut().select(v);
            settle(&mut h, None);
        } else if let Some(p) = l.get("shot").and_then(Value::as_str) {
            shot(&mut h, p)?;
        }
    }
    shot(&mut h, &out)
}
