//! The parity gate (ADR 0004): every case in `parity/observed/` runs on `cargo test`.

use std::path::Path;

#[test]
fn parity() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../parity");
    let report = opendash_model::parity::run(&root).unwrap();
    let problems = report.problems();
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}
