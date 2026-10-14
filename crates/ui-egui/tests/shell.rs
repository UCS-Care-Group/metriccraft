//! The window driven the way a screen reader or keyboard user would: every control found by its
//! AccessKit label (spec A-1), every action reaching the engine as a command.

use std::time::{Duration, Instant};

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use opendash_engine::App;
use opendash_ui_egui::worker::Engine;
use opendash_ui_egui::{Prefs, Shell};

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
    h.get_by_label("New page").click();
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
    assert_eq!(h.state().prefs().view, opendash_ui_egui::views::View::Model);
    h.get_by_label("View").click();
    h.run();
    h.get_by_label("Dark").click();
    h.run();
    assert_eq!(h.state().prefs().theme, opendash_ui_egui::theme::Mode::Dark);
}

#[test]
fn a_command_needing_params_opens_its_form() {
    let mut h = harness();
    h.get_by_label("Get Data").click();
    h.run();
    // The form names each param; Run stays disabled until the required ones are given.
    h.get_by_label("kind");
    h.get_by_label("path");
    h.get_by_label("Cancel").click();
    h.run();
    assert!(h.query_by_label("path").is_none());
}
