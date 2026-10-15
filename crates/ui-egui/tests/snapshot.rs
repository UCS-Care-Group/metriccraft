//! UI snapshots for `cargo xtask snapshot`: each View (and the Power Query editor), light and
//! dark, rendered offscreen to PNGs in `$MCRAFT_SNAPSHOT_PNG` (gitignored `observations/`), to
//! look at after UI work. Without that variable the test does nothing (no GPU needed in CI).

use std::time::{Duration, Instant};

use metriccraft_engine::App;
use metriccraft_ui_egui::worker::Engine;
use metriccraft_ui_egui::{Prefs, Shell};
use serde_json::json;

type Harness = egui_kittest::Harness<'static, Shell>;

fn settle(h: &mut Harness) {
    let end = Instant::now() + Duration::from_secs(10);
    while h.state().snapshot().busy.is_some() && Instant::now() < end {
        h.step();
        std::thread::sleep(Duration::from_millis(5));
    }
    for _ in 0..30 {
        h.step();
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn run(h: &mut Harness, id: &str, params: serde_json::Value) {
    let seq = h.state().snapshot().seq;
    h.state_mut().run(id, params);
    let end = Instant::now() + Duration::from_secs(10);
    while h.state().snapshot().seq == seq && Instant::now() < end {
        h.step();
        std::thread::sleep(Duration::from_millis(2));
    }
    settle(h);
}

#[test]
fn ui_snapshot_views() {
    let Some(out) = std::env::var_os("MCRAFT_SNAPSHOT_PNG").map(std::path::PathBuf::from) else {
        return;
    };
    std::fs::create_dir_all(&out).unwrap();
    let csv = out.join("ui-sales.csv");
    let mut text = String::from("Region,Product,Amount\n");
    for i in 0..300 {
        let region = ["North", "South", "West"][i % 3];
        let product = ["Apple", "Pear", "Plum"][(i / 3) % 3];
        text.push_str(&format!(
            "{region},{product},{}\n",
            (i * 7 % 100) as f64 / 4.0
        ));
    }
    std::fs::write(&csv, text).unwrap();

    let shell = Shell::new(
        Engine::new(App::new(), || {}),
        Prefs::default(),
        (None, Vec::new()),
    );
    let mut h = egui_kittest::Harness::builder()
        .with_size(egui::vec2(1440.0, 900.0))
        .wgpu()
        .build_ui_state(
            |ui, shell: &mut Shell| {
                shell.logic(ui.ctx());
                shell.ui(ui);
            },
            shell,
        );
    settle(&mut h);
    run(
        &mut h,
        "home.getData",
        json!({"kind": "csv", "path": csv, "name": "Sales"}),
    );
    run(&mut h, "insert.newPage", json!({}));
    for (kind, x, field, well) in [
        ("clusteredColumn", 20, "Region", "axis"),
        ("table", 640, "Product", "values"),
    ] {
        run(
            &mut h,
            "insert.newVisual",
            json!({"type": kind, "x": x, "y": 20, "w": 580, "h": 320}),
        );
        let page = h.state().snapshot().doc.report.pages[0].clone();
        let id = page
            .visuals
            .iter()
            .max_by_key(|v| v.x)
            .map(|v| v.id.clone())
            .unwrap();
        for (f, w) in [(field, well), ("Amount", "values")] {
            run(
                &mut h,
                "visual.addField",
                json!({"page": page.id, "visual": id,
                "well": w, "field": {"table": "Sales", "column": f}}),
            );
        }
    }

    for (dark, theme) in [(false, "light"), (true, "dark")] {
        if dark {
            let ctx = h.ctx.clone();
            h.state_mut().set_dark(&ctx);
        }
        for view in ["report", "table", "model"] {
            h.state_mut().set_view(view);
            settle(&mut h);
            let img = h.render().unwrap();
            img.save(out.join(format!("ui-{view}-{theme}.png")))
                .unwrap();
        }
    }
    h.state_mut().set_view("report");
    h.state_mut().open_query_editor(Some("Sales"));
    settle(&mut h);
    let img = h.render().unwrap();
    img.save(out.join("ui-power-query-dark.png")).unwrap();
    std::fs::remove_file(&csv).unwrap();
}
