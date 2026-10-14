//! `docs/commands.md` is generated from the command table; this keeps it current.

use std::path::Path;

#[test]
fn commands_doc_is_current() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/commands.md");
    let want = opendash_engine::reference();
    if std::env::var_os("ODASH_SNAPSHOT_UPDATE").is_some() {
        std::fs::write(&path, &want).unwrap();
    }
    let have = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        have == want,
        "docs/commands.md is stale: run ODASH_SNAPSHOT_UPDATE=1 cargo test -p opendash-engine commands_doc"
    );
}
