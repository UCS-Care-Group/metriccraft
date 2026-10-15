//! The A-1 audit: in every View, Pane and dialog, every control a keyboard or screen-reader
//! user can act on carries a label that says what it does (words, not just a symbol).

use std::time::{Duration, Instant};

use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use metriccraft_engine::App;
use metriccraft_ui_egui::worker::Engine;
use metriccraft_ui_egui::{Prefs, Shell};
use serde_json::json;

const INTERACTIVE: [Role; 12] = [
    Role::Button,
    Role::CheckBox,
    Role::RadioButton,
    Role::TextInput,
    Role::MultilineTextInput,
    Role::ComboBox,
    Role::Slider,
    Role::Tab,
    Role::MenuItem,
    Role::Link,
    Role::SpinButton,
    Role::Switch,
];

fn harness() -> Harness<'static, Shell> {
    let shell = Shell::new(
        Engine::new(App::new(), || {}),
        Prefs::default(),
        (None, Vec::new()),
    );
    let mut h = Harness::builder()
        .with_size(egui::vec2(1440.0, 900.0))
        .build_ui_state(
            |ui, shell: &mut Shell| {
                shell.logic(ui.ctx());
                shell.ui(ui);
            },
            shell,
        );
    h.run();
    h
}

/// Runs `id` and waits for the engine.
fn run(h: &mut Harness<'_, Shell>, id: &str, params: serde_json::Value) {
    let seq = h.state().snapshot().seq;
    h.state_mut().run(id, params);
    let end = Instant::now() + Duration::from_secs(10);
    while h.state().snapshot().seq == seq && Instant::now() < end {
        h.step();
        std::thread::sleep(Duration::from_millis(2));
    }
    for _ in 0..30 {
        h.step();
    }
}

/// The interactive controls without a worded label, described for the failure message.
fn unlabelled(h: &Harness<'_, Shell>, state: &str) -> Vec<String> {
    let bad = |n: &egui_kittest::kittest::AccessKitNode<'_>| {
        let words = n
            .label()
            .or_else(|| n.value())
            .is_some_and(|l| l.chars().any(char::is_alphabetic));
        INTERACTIVE.contains(&n.role()) && !n.is_hidden() && !words
    };
    h.query_all_by(bad)
        .map(|n| format!("{state}: {:?} at {:?}", n.accesskit_node().role(), n.rect()))
        .collect()
}

#[test]
fn every_control_is_labelled_in_every_view_and_dialog() {
    let dir = std::env::temp_dir().join(format!("mcraft-a11y-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap_or_default();
    let csv = dir.join("Sales.csv");
    let mut text = String::from("Region,Amount\n");
    for i in 0..300 {
        text.push_str(&format!("R{},{i}\n", i % 3));
    }
    std::fs::write(&csv, text).unwrap_or_default();

    let mut h = harness();
    let mut problems = unlabelled(&h, "empty");
    run(&mut h, "home.getData", json!({"kind": "csv", "path": csv}));
    run(&mut h, "insert.newPage", json!({}));
    run(&mut h, "insert.newVisual", json!({"type": "slicer"}));
    let visual = h.state().snapshot().doc.report.pages[0].visuals[0]
        .id
        .clone();
    run(
        &mut h,
        "visual.addField",
        json!({"page": "Page 1", "visual": visual, "well": "field", "field": {"table": "Sales", "column": "Region"}}),
    );
    run(
        &mut h,
        "slicer.select",
        json!({"page": "Page 1", "visual": visual, "items": ["R1"]}),
    );
    h.state_mut().select(&visual);
    h.run();
    problems.extend(unlabelled(&h, "report, visual selected"));

    for view in ["table", "model"] {
        h.state_mut().set_view(view);
        h.run();
        problems.extend(unlabelled(&h, view));
    }
    h.state_mut().set_view("report");

    h.state_mut().invoke("modeling.newMeasure");
    h.run();
    problems.extend(unlabelled(&h, "formula bar"));
    h.get_by_label("Cancel").click();
    h.run();

    h.state_mut().invoke("visual.resize");
    h.run();
    problems.extend(unlabelled(&h, "form"));
    h.key_press(egui::Key::Escape);
    h.run();

    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::K);
    h.run();
    problems.extend(unlabelled(&h, "command palette"));
    h.key_press(egui::Key::Escape);
    h.run();

    h.state_mut().open_query_editor(Some("Sales"));
    for _ in 0..50 {
        h.step();
        std::thread::sleep(Duration::from_millis(5));
    }
    problems.extend(unlabelled(&h, "Power Query editor"));

    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        problems.is_empty(),
        "unlabelled controls:\n{}",
        problems.join("\n")
    );
}
