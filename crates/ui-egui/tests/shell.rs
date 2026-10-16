//! The window driven the way a screen reader or keyboard user would: every control found by its
//! AccessKit label, every action reaching the engine as a command.

use std::time::{Duration, Instant};

use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use metriccraft_engine::App;
use metriccraft_ui_egui::worker::Engine;
use metriccraft_ui_egui::{Prefs, Shell};

fn harness() -> Harness<'static, Shell> {
    let shell = Shell::new(
        Engine::new(App::new(), || {}),
        Prefs::default(),
        (None, Vec::new()),
    );
    let mut h = Harness::builder()
        .with_size(egui::vec2(1280.0, 800.0))
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

/// Steps frames until `done` or 10 s.
fn wait(h: &mut Harness<'_, Shell>, done: impl Fn(&Shell) -> bool) {
    let end = Instant::now() + Duration::from_secs(10);
    while !done(h.state()) && Instant::now() < end {
        h.step();
        std::thread::sleep(Duration::from_millis(5));
    }
    h.step();
    assert!(done(h.state()), "timed out");
}

#[test]
fn ribbon_buttons_run_commands_and_undo_reverts() {
    let mut h = harness();
    h.get_by_label("Insert").click();
    h.run();
    // The Ribbon's New page (the page tabs' + is New page too).
    h.get_all_by_label("New page").next().unwrap().click();
    h.run();
    wait(&mut h, |s| s.snapshot().doc.report.pages.len() == 1);
    assert!(h.state().snapshot().dirty);

    h.get_by_label("Undo").click();
    h.run();
    wait(&mut h, |s| s.snapshot().doc.report.pages.is_empty());
}

#[test]
fn views_and_theme_switch() {
    let mut h = harness();
    h.get_by_label("Model").click();
    h.run();
    assert_eq!(
        h.state().prefs().view,
        metriccraft_ui_egui::views::View::Model
    );
    h.get_by_label("View").click();
    h.run();
    h.get_by_label("Dark").click();
    h.run();
    assert_eq!(
        h.state().prefs().theme,
        metriccraft_ui_egui::theme::Mode::Dark
    );
}

#[test]
fn a_command_needing_params_opens_its_form() {
    let mut h = harness();
    h.get_by_label("Get Data").click();
    h.run();
    // The form names each param; Run stays disabled until the required ones are given.
    h.get_by_role_and_label(Role::ComboBox, "kind");
    h.get_by_role_and_label(Role::TextInput, "path");
    h.get_by_label("Cancel").click();
    h.run();
    assert!(h.query_by_role_and_label(Role::TextInput, "path").is_none());
}

/// The current Page's one Visual's (x, y, w, h).
fn geometry(s: &Shell) -> Option<(i32, i32, i32, i32)> {
    let page = s.snapshot().doc.report.pages.first()?;
    let v = page.visuals.first()?;
    Some((v.x, v.y, v.w, v.h))
}

/// Drags with the primary button from `from` to `to` in a few steps.
fn drag(h: &mut Harness<'_, Shell>, from: egui::Pos2, to: egui::Pos2) {
    let button = |pos, pressed| egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    h.input_mut().events.push(egui::Event::PointerMoved(from));
    h.step();
    h.input_mut().events.push(button(from, true));
    h.step();
    for i in 1..=5 {
        let at = from + (to - from) * (i as f32 / 5.0);
        h.input_mut().events.push(egui::Event::PointerMoved(at));
        h.step();
    }
    h.input_mut().events.push(button(to, false));
    h.run();
}

#[test]
fn canvas_selects_drags_nudges_and_deletes() {
    let mut h = harness();
    h.state_mut().run(
        "insert.newVisual",
        serde_json::json!({"type": "card", "x": 100, "y": 100}),
    );
    wait(&mut h, |s| geometry(s) == Some((100, 100, 400, 300)));

    assert_eq!(h.state().zoom(), metriccraft_ui_egui::canvas::Zoom::FitPage);
    // Clicking selects; the Visualizations pane shows its wells.
    h.get_by_label("Card visual").click();
    h.run();
    assert!(h.state().selected().is_some());
    h.get_by_label("Add data fields here");

    // A drag is one command, sent on release.
    let r = h.get_by_label("Card visual").rect();
    drag(&mut h, r.center(), r.center() + egui::vec2(50.0, 0.0));
    wait(&mut h, |s| {
        geometry(s).is_some_and(|g| g.0 > 100 && g.1 == 100)
    });
    let moved = geometry(h.state()).map(|g| g.0).unwrap_or_default();

    // Two nudges merge into one undo step.
    h.key_press(egui::Key::ArrowRight);
    h.run();
    h.key_press(egui::Key::ArrowRight);
    h.run();
    wait(&mut h, |s| geometry(s).is_some_and(|g| g.0 == moved + 2));
    h.state_mut().run("edit.undo", serde_json::json!({}));
    wait(&mut h, |s| geometry(s).is_some_and(|g| g.0 == moved));

    h.key_press(egui::Key::Delete);
    h.run();
    wait(&mut h, |s| geometry(s).is_none());
    assert!(h.state().selected().is_none());
}

#[test]
fn table_view_scrolls_by_keyboard() {
    let dir = std::env::temp_dir().join(format!("mcraft-grid-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let csv = dir.join("t.csv");
    let mut text = String::from("N\n");
    for i in 0..5000 {
        text.push_str(&format!("{i}\n"));
    }
    std::fs::write(&csv, text).unwrap();
    let mut h = harness();
    h.state_mut().run(
        "home.getData",
        serde_json::json!({"kind": "csv", "path": csv, "name": "T"}),
    );
    wait(&mut h, |s| !s.snapshot().doc.tables.is_empty());
    h.state_mut().set_view("table");
    h.run();
    h.get_by_label("T table").click();
    h.run();
    h.key_press(egui::Key::End);
    h.run();
    let top = h.state().grid().top;
    assert!(top > 4900 && top < 5000, "{top}");
    h.key_press(egui::Key::PageUp);
    h.run();
    assert!(h.state().grid().top < top);
    h.key_press(egui::Key::Home);
    h.run();
    assert_eq!(h.state().grid().top, 0);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn model_view_moves_tables_and_relates_columns_dragged_either_way() {
    let dir = std::env::temp_dir().join(format!("mcraft-model-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut h = harness();
    h.state_mut().run(
        "options.autodetectRelationships",
        serde_json::json!({"enabled": false}),
    );
    for (name, text) in [
        ("Facts", "Key,Val\n1,10\n1,20\n2,5\n"),
        ("Keys", "Key,Size\n1,3\n2,4\n"),
    ] {
        let path = dir.join(format!("{name}.csv"));
        std::fs::write(&path, text).unwrap();
        h.state_mut().run(
            "home.getData",
            serde_json::json!({"kind": "csv", "path": path, "name": name}),
        );
    }
    wait(&mut h, |s| s.snapshot().doc.model.tables.len() == 2);
    h.state_mut().set_view("model");
    h.run();

    let head = h.get_by_label("Keys table").rect().center();
    drag(&mut h, head, head + egui::vec2(0.0, 120.0));
    wait(&mut h, |s| s.snapshot().doc.diagram.contains_key("Keys"));

    // From the one side to the many side: the ends are swapped.
    let from = h.get_by_label("Key column of Keys").rect().center();
    let to = h.get_by_label("Key column of Facts").rect().center();
    drag(&mut h, from, to);
    wait(&mut h, |s| s.snapshot().doc.model.relationships.len() == 1);
    let r = &h.state().snapshot().doc.model.relationships[0];
    assert_eq!(
        (r.from_table.as_str(), r.to_table.as_str()),
        ("Facts", "Keys")
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn new_measure_opens_the_formula_bar_and_enter_commits() {
    let dir = std::env::temp_dir().join(format!("mcraft-dax-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("T.csv");
    std::fs::write(&path, "N\n1\n2\n").unwrap();
    let mut h = harness();
    h.state_mut().run(
        "home.getData",
        serde_json::json!({"kind": "csv", "path": path, "name": "T"}),
    );
    wait(&mut h, |s| !s.snapshot().doc.model.tables.is_empty());
    h.get_by_label("Modeling").click();
    h.run();
    h.get_by_label("New measure").click();
    h.run();
    h.get_by_label("Measure formula").type_text("SUM(");
    h.run();
    // A parse error shows while typing; nothing is committed.
    h.get_by_label_contains("expected an expression");
    h.get_by_label("Measure formula").type_text("T[N])");
    h.run();
    h.key_press(egui::Key::Enter);
    h.run();
    wait(&mut h, |s| {
        s.snapshot().doc.model.tables.first().is_some_and(|t| {
            t.measures
                .iter()
                .any(|m| m.name == "Measure" && m.expression == "SUM(T[N])")
        })
    });
    assert!(h.query_by_label("Measure formula").is_none());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_running_load_shows_rows_read_and_cancels() {
    let dir = std::env::temp_dir().join(format!("mcraft-cancel-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("Big.csv");
    let mut text = String::from("Id,Name\n");
    for i in 0..3_000_000 {
        text.push_str(&format!("{i},name {i}\n"));
    }
    std::fs::write(&path, text).unwrap();
    let mut h = harness();
    h.state_mut().run(
        "home.getData",
        serde_json::json!({"kind": "csv", "path": path}),
    );
    // Rows read show while it runs; Cancel stops it.
    let end = Instant::now() + Duration::from_secs(10);
    while h.query_by_label_contains("rows read").is_none() && Instant::now() < end {
        h.step();
        std::thread::sleep(Duration::from_millis(2));
    }
    h.get_by_label("Cancel").click();
    h.step();
    wait(&mut h, |s| s.snapshot().busy.is_none());
    assert!(h.state().snapshot().doc.tables.is_empty());
    assert!(
        h.state()
            .toasts()
            .iter()
            .any(|t| t.text.contains("cancelled"))
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn get_data_picks_a_file_then_shows_its_source_form() {
    let dir = std::env::temp_dir().join(format!("mcraft-getdata-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("Sales.csv");
    std::fs::write(&path, "Region,Qty\nNorth,3\n").unwrap();
    let picked = path.clone();
    let shell = Shell::new(
        Engine::new(App::new(), || {}),
        Prefs::default(),
        (None, Vec::new()),
    )
    .with_file_dialogs(Box::new(move |_| {
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(Some(picked.clone())).unwrap();
        rx
    }));
    let mut h = Harness::builder()
        .with_size(egui::vec2(1280.0, 800.0))
        .build_ui_state(
            |ui, shell: &mut Shell| {
                shell.logic(ui.ctx());
                shell.ui(ui);
            },
            shell,
        );
    h.run();
    h.get_by_label("Get Data").click();
    h.run();
    // The form opens with the file and its kind filled in.
    h.get_by_label("Run").click();
    // The status bar polls the load's progress while it runs: step, don't wait for stillness.
    h.step();
    wait(&mut h, |s| s.snapshot().doc.tables.len() == 1);
    assert_eq!(h.state().snapshot().doc.tables[0].name(), "Sales");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn power_query_editor_previews_steps_and_closes() {
    let dir = std::env::temp_dir().join(format!("mcraft-pq-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("Sales.csv");
    // 200 good rows (the type detection sample), then a Qty that isn't a number.
    let mut text = String::from("Region,Qty\n");
    for i in 0..200 {
        text.push_str(&format!("North,{i}\n"));
    }
    std::fs::write(&path, text + "South,x\n").unwrap();
    let mut h = harness();
    h.state_mut().run(
        "home.getData",
        serde_json::json!({"kind": "csv", "path": path}),
    );
    wait(&mut h, |s| s.snapshot().doc.queries.len() == 1);
    h.get_by_label("Transform data").click();
    h.run();
    assert!(h.state().query_editor_open());
    h.get_by_label("Applied Steps");
    h.get_by_label("Promoted Headers");
    // The Preview comes from the worker; the bad Qty shows as a Cell error.
    let end = Instant::now() + Duration::from_secs(10);
    while h.query_by_label("Sales preview").is_none() && Instant::now() < end {
        h.step();
        std::thread::sleep(Duration::from_millis(5));
    }
    h.get_by_label("Sales preview");
    h.get_by_label("Changed Type");
    h.get_by_label("Source").click();
    h.run();
    h.get_by_label("Close editor").click();
    h.run();
    assert!(!h.state().query_editor_open());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn f6_cycles_focus_ribbon_view_panes() {
    use metriccraft_ui_egui::keys::region_id;
    let mut h = harness();
    let focused = |h: &Harness<'_, Shell>| h.ctx.memory(|m| m.focused());
    for n in [0, 1, 2, 0] {
        h.key_press(egui::Key::F6);
        h.run();
        assert_eq!(focused(&h), Some(region_id(n)), "region {n}");
    }
    h.key_press_modifiers(egui::Modifiers::SHIFT, egui::Key::F6);
    h.run();
    assert_eq!(focused(&h), Some(region_id(2)));
    // Tab goes on from a region's anchor to its first control.
    h.key_press(egui::Key::Tab);
    h.run();
    let now = focused(&h);
    assert!(now.is_some() && now != Some(region_id(2)));
    // Each region's anchor is labelled for screen readers.
    for label in ["Ribbon", "Panes"] {
        h.get_by_label(label);
    }
}

#[test]
fn alt_key_tips_open_a_tab_and_run_a_button() {
    let mut h = harness();
    // Alt pressed and released alone shows the tabs' key tips.
    for m in [egui::Modifiers::ALT, egui::Modifiers::NONE] {
        h.input_mut().events.push(egui::Event::ModifiersChanged(m));
        h.step();
    }
    // N: Insert; A: its first button, New page.
    h.key_press(egui::Key::N);
    h.run();
    h.key_press(egui::Key::A);
    h.run();
    wait(&mut h, |s| s.snapshot().doc.report.pages.len() == 1);
}

#[test]
fn a_visual_with_text_draws_without_an_explicit_setup() {
    // Regression: painting a Scene's text before `Shell::setup` registered Inter panicked.
    let dir = std::env::temp_dir().join(format!("mcraft-fonts-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("T.csv");
    std::fs::write(&path, "N\n1\n2\n").unwrap();
    let mut h = harness();
    // Wait on outcomes: "not busy" holds before the worker has picked a job up.
    h.state_mut().run(
        "home.getData",
        serde_json::json!({"kind": "csv", "path": path}),
    );
    wait(&mut h, |s| !s.snapshot().doc.tables.is_empty());
    h.state_mut()
        .run("insert.newVisual", serde_json::json!({"type": "card"}));
    wait(&mut h, |s| s.snapshot().doc.report.pages.len() == 1);
    let page = h.state().snapshot().doc.report.pages[0].clone();
    let params = serde_json::json!({"page": page.id, "visual": page.visuals[0].id,
        "well": "values", "field": {"table": "T", "column": "N"}});
    h.state_mut().run("visual.addField", params);
    wait(&mut h, |s| !s.snapshot().scenes.1.is_empty());
    h.run();
    std::fs::remove_dir_all(&dir).unwrap();
}
