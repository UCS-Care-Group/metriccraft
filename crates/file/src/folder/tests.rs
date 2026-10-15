use super::*;

/// A fresh, empty folder for one test; removed when dropped.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("metriccraft-file-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// A file set whose Manifest lists `parts` with their hashes.
fn files(parts: &[(&str, &str)]) -> FileSet {
    let mut f = FileSet::new();
    let mut m = Manifest {
        version: FORMAT_VERSION,
        ..Manifest::default()
    };
    for (p, text) in parts {
        f.insert(p.to_string(), text.as_bytes().to_vec());
        m.parts.insert(p.to_string(), hash(text.as_bytes()));
    }
    f.insert(MANIFEST.into(), m.print());
    f
}

fn with_gitignore(mut f: FileSet) -> FileSet {
    f.insert(GITIGNORE.into(), GITIGNORE_TEXT.as_bytes().to_vec());
    f
}

fn v1() -> FileSet {
    files(&[
        ("model/model.mcm", "model Model\n"),
        ("queries/A.mcq", "query A\n"),
        ("queries/B.mcq", "query B\n"),
    ])
}

fn v2() -> FileSet {
    files(&[
        ("model/model.mcm", "model Model\n  culture: en-GB\n"),
        ("queries/A.mcq", "query A\n  load: false\n"),
        ("queries/C.mcq", "query C\n"),
    ])
}

#[test]
fn write_then_read_gives_the_same_files() {
    let dir = TempDir::new("write-read");
    let root = dir.0.join("Sales");
    write_folder(&root, &v1()).unwrap();
    assert_eq!(
        read_folder(&root, Recover::No).unwrap(),
        with_gitignore(v1())
    );
    assert!(!root.join(STAGING).exists(), "staging is cleaned up");

    write_folder(&root, &v2()).unwrap();
    assert_eq!(
        read_folder(&root, Recover::No).unwrap(),
        with_gitignore(v2())
    );
    assert!(
        !root.join("queries/B.mcq").exists(),
        "a part no longer listed is removed"
    );
}

#[test]
fn gitignore_is_written_on_create_only() {
    let dir = TempDir::new("gitignore");
    let root = dir.0.join("P");
    write_folder(&root, &v1()).unwrap();
    assert_eq!(
        fs::read(root.join(GITIGNORE)).unwrap(),
        GITIGNORE_TEXT.as_bytes()
    );
    fs::write(root.join(GITIGNORE), "mine\n").unwrap();
    write_folder(&root, &v2()).unwrap();
    assert_eq!(fs::read(root.join(GITIGNORE)).unwrap(), b"mine\n");
}

/// Replays a save of `v2` over `v1` that stops after moving `queries/A.mcq` into place.
fn interrupted(root: &Path) {
    write_folder(root, &v1()).unwrap();
    let new = v2();
    let staging = root.join(STAGING);
    write_file(&staging.join(MANIFEST), &new[MANIFEST]).unwrap();
    for part in ["model/model.mcm", "queries/A.mcq", "queries/C.mcq"] {
        write_file(&join(&staging.join("new"), part), &new[part]).unwrap();
    }
    rename(
        &root.join("queries/A.mcq"),
        &staging.join("old/queries/A.mcq"),
    )
    .unwrap();
    rename(
        &staging.join("new/queries/A.mcq"),
        &root.join("queries/A.mcq"),
    )
    .unwrap();
}

#[test]
fn an_interrupted_save_is_detected_and_either_state_recovered() {
    let dir = TempDir::new("interrupted");
    let root = dir.0.join("P");
    interrupted(&root);
    assert_eq!(
        read_folder(&root, Recover::No),
        Err(Error::Interrupted {
            parts: vec!["queries/A.mcq".into()],
            staging: true
        })
    );
    assert_eq!(
        read_folder(&root, Recover::LastGood).unwrap(),
        with_gitignore(v1()),
        "the state before the save"
    );
    assert_eq!(
        read_folder(&root, Recover::Staging).unwrap(),
        with_gitignore(v2()),
        "the save being made"
    );
    // Saving the recovered state makes the folder consistent again.
    write_folder(&root, &v1()).unwrap();
    assert_eq!(
        read_folder(&root, Recover::No).unwrap(),
        with_gitignore(v1())
    );
    assert!(!root.join(STAGING).exists());
}

#[test]
fn an_interrupt_mid_move_is_still_recoverable() {
    // Stopped between moving the old A.mcq out and the new one in: A.mcq is missing.
    let dir = TempDir::new("mid-move");
    let root = dir.0.join("P");
    write_folder(&root, &v1()).unwrap();
    let staging = root.join(STAGING);
    write_file(&staging.join(MANIFEST), &v2()[MANIFEST]).unwrap();
    for part in ["model/model.mcm", "queries/A.mcq", "queries/C.mcq"] {
        write_file(&join(&staging.join("new"), part), &v2()[part]).unwrap();
    }
    rename(
        &root.join("queries/A.mcq"),
        &staging.join("old/queries/A.mcq"),
    )
    .unwrap();
    assert!(matches!(
        read_folder(&root, Recover::No),
        Err(Error::Interrupted { staging: true, .. })
    ));
    assert_eq!(
        read_folder(&root, Recover::LastGood).unwrap(),
        with_gitignore(v1())
    );
    assert_eq!(
        read_folder(&root, Recover::Staging).unwrap(),
        with_gitignore(v2())
    );
}

#[test]
fn a_hand_edited_part_without_staging_offers_only_damage() {
    let dir = TempDir::new("hand-edit");
    let root = dir.0.join("P");
    write_folder(&root, &v1()).unwrap();
    fs::write(root.join("queries/B.mcq"), "query B\n  load: false\n").unwrap();
    assert_eq!(
        read_folder(&root, Recover::No),
        Err(Error::Interrupted {
            parts: vec!["queries/B.mcq".into()],
            staging: false
        })
    );
    assert!(matches!(
        read_folder(&root, Recover::Staging),
        Err(Error::Damaged { .. })
    ));
}

#[test]
fn not_a_project_and_unsafe_manifests_are_errors() {
    let dir = TempDir::new("not-project");
    assert!(matches!(
        read_folder(&dir.0, Recover::No),
        Err(Error::NotAProject(_))
    ));
    let h = hash(b"x");
    fs::write(
        dir.0.join(MANIFEST),
        format!("{{\"formatVersion\": 1, \"parts\": {{\"../escape\": \"{h}\"}}}}"),
    )
    .unwrap();
    assert!(matches!(
        read_folder(&dir.0, Recover::No),
        Err(Error::Manifest(_))
    ));
    fs::write(dir.0.join(MANIFEST), "{\"formatVersion\": 7}").unwrap();
    assert!(matches!(
        read_folder(&dir.0, Recover::No),
        Err(Error::NewerFormat { found: 7, .. })
    ));
}

#[test]
fn packages_are_written_atomically_and_read_back() {
    let dir = TempDir::new("package");
    let path = dir.0.join("Sales.mcraft");
    let f = with_gitignore(v1());
    write_package(&path, &f).unwrap();
    let first = fs::read(&path).unwrap();
    assert!(!dir.0.join("Sales.mcraft.tmp").exists());
    assert_eq!(read_package(&path).unwrap(), f);
    write_package(&path, &f).unwrap();
    assert_eq!(fs::read(&path).unwrap(), first, "byte-identical");

    // Package → folder → Package.
    let root = dir.0.join("Unpacked");
    write_folder(&root, &read_package(&path).unwrap()).unwrap();
    let again = dir.0.join("Again.mcraft");
    write_package(&again, &read_folder(&root, Recover::No).unwrap()).unwrap();
    assert_eq!(fs::read(&again).unwrap(), first);
}
