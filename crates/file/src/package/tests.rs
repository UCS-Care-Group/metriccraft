use super::*;

fn files() -> FileSet {
    let mut f = FileSet::new();
    f.insert("project.mcraft.json".into(), b"{}\n".to_vec());
    f.insert("queries/Sales.mcq".into(), b"query Sales\n".repeat(100));
    f.insert(".gitignore".into(), b".cache/\n".to_vec());
    f.insert(".cache/Sales.parquet".into(), vec![0, 1, 2, 3]);
    f.insert("empty.txt".into(), Vec::new());
    f
}

/// A zip with these raw entries, written without our checks.
fn raw_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    for (name, bytes) in entries {
        zip.start_file(*name, SimpleFileOptions::default()).unwrap();
        zip.write_all(bytes).unwrap();
    }
    zip.finish().unwrap().into_inner()
}

#[test]
fn project_package_project_is_byte_identical() {
    let f = files();
    let a = pack(&f).unwrap();
    assert_eq!(a, pack(&f).unwrap(), "same files, same bytes");
    let back = unpack(&a).unwrap();
    assert_eq!(back, f);
    assert_eq!(pack(&back).unwrap(), a);
}

#[test]
fn entries_are_sorted_dated_1980_and_parquet_is_stored() {
    let bytes = pack(&files()).unwrap();
    let mut zip = ZipArchive::new(Cursor::new(bytes.as_slice())).unwrap();
    let names: Vec<String> = (0..zip.len())
        .map(|i| zip.by_index(i).unwrap().name().to_string())
        .collect();
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(names, sorted);
    for i in 0..zip.len() {
        let e = zip.by_index(i).unwrap();
        assert_eq!(e.last_modified(), Some(DateTime::DEFAULT));
        let want = if e.name().ends_with(".parquet") {
            CompressionMethod::Stored
        } else {
            CompressionMethod::Deflated
        };
        assert_eq!(e.compression(), want, "{}", e.name());
        assert_eq!(e.unix_mode().map(|m| m & 0o777), Some(0o644));
    }
}

#[test]
fn local_overrides_are_not_packed() {
    let mut f = files();
    f.insert(".local/paths.json".into(), b"{}".to_vec());
    let back = unpack(&pack(&f).unwrap()).unwrap();
    assert_eq!(back, files());
}

#[test]
fn path_traversal_is_refused() {
    for name in [
        "../evil",
        "a/../../evil",
        "/etc/passwd",
        "a\\..\\b",
        "C:/x",
        "./a",
        "a//b",
        "a/./b",
    ] {
        let zip = raw_zip(&[("project.mcraft.json", b"{}"), (name, b"x")]);
        let e = unpack(&zip).expect_err(name);
        assert!(matches!(e, Error::Package(_)), "{name}: {e}");
    }
}

#[test]
fn zip_bombs_and_huge_entry_counts_are_refused() {
    let zeros = vec![0u8; 4 << 20];
    let zip = raw_zip(&[("a", &zeros), ("b", &zeros)]);
    assert!(zip.len() < 64 << 10, "compresses well: {}", zip.len());
    let small = Limits {
        entries: 10,
        bytes: 6 << 20,
        cache: 6 << 20,
    };
    let e = unpack_with(&zip, small).unwrap_err();
    assert!(e.to_string().contains("uncompressed"), "{e}");
    let roomy = Limits {
        bytes: 8 << 20,
        ..small
    };
    assert!(unpack_with(&zip, roomy).is_ok());

    let names: Vec<String> = (0..11).map(|i| format!("f{i}")).collect();
    let entries: Vec<(&str, &[u8])> = names.iter().map(|n| (n.as_str(), &b""[..])).collect();
    let e = unpack_with(&raw_zip(&entries), small).unwrap_err();
    assert!(e.to_string().contains("entries"), "{e}");
}

#[test]
fn duplicates_and_garbage_are_refused() {
    let zip = raw_zip(&[("a.txt", b"1"), ("A.TXT", b"2")]);
    assert!(unpack(&zip).is_err());
    assert!(unpack(b"not a zip").is_err());
    assert!(unpack(&[]).is_err());
}

#[test]
fn mutated_packages_never_panic() {
    let good = pack(&files()).unwrap();
    let mut state = 0x1234_5678_9abc_def1u64;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for _ in 0..2000 {
        let mut bytes = good.clone();
        for _ in 0..1 + next() % 4 {
            let at = (next() as usize) % bytes.len();
            bytes[at] = next() as u8;
        }
        if next() % 5 == 0 {
            let len = (next() as usize) % bytes.len();
            bytes.truncate(len);
        }
        let _ = unpack(&bytes);
    }
}
