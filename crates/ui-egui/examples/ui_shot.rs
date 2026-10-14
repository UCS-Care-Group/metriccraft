//! Headless screenshots of the window (wgpu, offscreen): what the UI looks like without opening
//! a window on anyone's screen.
//!
//! `cargo run -p opendash-ui-egui --example ui_shot -- OUT.png [SCRIPT.jsonl]`
//!
//! The script is JSON lines, run in order: `{"run": "<command>", "params": {…}}` runs a command
//! and waits for it; `{"dark": true}` switches to the dark App theme; `{"view": "table"}` picks a
//! View; `{"shot": "/abs/out.png"}` saves a PNG there too. The window is 1440×900 pt at 2×.

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
    for line in script.lines().filter(|l| !l.trim().is_empty()) {
        let l: Value = serde_json::from_str(line).map_err(|e| format!("{line}: {e}"))?;
        if let Some(id) = l.get("run").and_then(Value::as_str) {
            let seq = h.state().snapshot().seq + 1;
            let params = l.get("params").cloned().unwrap_or(Value::Null);
            h.state_mut().run(id, params);
            settle(&mut h, Some(seq));
        } else if l.get("dark").and_then(Value::as_bool) == Some(true) {
            let ctx = h.ctx.clone();
            h.state_mut().set_dark(&ctx);
            settle(&mut h, None);
        } else if let Some(v) = l.get("view").and_then(Value::as_str) {
            h.state_mut().set_view(v);
            settle(&mut h, None);
        } else if let Some(p) = l.get("shot").and_then(Value::as_str) {
            shot(&mut h, p)?;
        }
    }
    shot(&mut h, &out)
}
