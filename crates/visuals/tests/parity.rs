//! The visual parity gate (ADR 0004, M4.6): every case parses and runs on our side, and the
//! ratchet in `parity/visuals/passing.txt` holds.

use std::path::Path;

#[test]
fn visual_parity() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../parity/visuals");
    let report = metriccraft_visuals::parity::run(&dir).unwrap();
    let problems = report.problems();
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[test]
fn every_visual_case_runs_on_our_side() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../parity/visuals");
    let cases = metriccraft_model::parity::cases_in(&dir).unwrap();
    assert!(cases.len() >= 4);
    for case in &cases {
        let (columns, rows) = metriccraft_visuals::parity::evaluate(case)
            .unwrap_or_else(|e| panic!("{}: {e}", case.id));
        assert!(!columns.is_empty() && !rows.is_empty(), "{}", case.id);
        eprintln!("{}: {columns:?}\n{rows:?}", case.id);
    }
}
