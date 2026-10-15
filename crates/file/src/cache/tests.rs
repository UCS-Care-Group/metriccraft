use arrow_array::{BooleanArray, Float64Array, Int64Array, StringArray};
use metriccraft_data::TableBuilder;
use metriccraft_data::culture::Culture;
use metriccraft_data::source::Bytes;
use metriccraft_data::step::{Step, fixed_array};

use super::*;

/// `unix` as an absolute path on this platform (`C:` in front on Windows, where `/p` is relative).
fn abs(unix: &str) -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(format!("C:{}", unix.replace('/', "\\")))
    } else {
        PathBuf::from(unix)
    }
}

/// A table as a load leaves it: text, whole, decimal, fixed decimal (narrow and wide), boolean.
fn table() -> Table {
    let batch = RecordBatch::try_from_iter_with_nullable([
        (
            "Region",
            Arc::new(StringArray::from(vec![Some("North"), None, Some("South")])) as ArrayRef,
            true,
        ),
        (
            "Qty",
            Arc::new(Int64Array::from(vec![3, 4, 5])) as ArrayRef,
            true,
        ),
        (
            "Price",
            Arc::new(Float64Array::from(vec![Some(1.5), None, Some(-1e300)])) as ArrayRef,
            true,
        ),
        (
            "Cost",
            fixed_array(vec![Some(15_000), None, Some(-1)]),
            true,
        ),
        (
            "Big",
            fixed_array(vec![Some(i64::MAX), Some(i64::MIN), Some(0)]),
            true,
        ),
        (
            "Ok",
            Arc::new(BooleanArray::from(vec![Some(true), Some(false), None])) as ArrayRef,
            true,
        ),
    ])
    .unwrap();
    let mut b = TableBuilder::new("Sales", batch.schema(), usize::MAX);
    b.push(&batch).unwrap();
    b.finish().unwrap()
}

fn model(t: &Table) -> ModelTable {
    ModelTable::load(None, t.clone(), Culture::get("en-US").unwrap(), usize::MAX)
        .unwrap()
        .0
}

fn provenance() -> Provenance {
    Provenance {
        m: hash(b"m"),
        sources: vec![
            Fingerprint::File {
                path: abs("/p/data/Sales.csv"),
                stat: Some((52, 1_760_000_000_123_456_789)),
            },
            Fingerprint::File {
                path: abs("/elsewhere/gone.csv"),
                stat: None,
            },
            Fingerprint::Bytes {
                name: "x.csv".into(),
                sha256: hash(b"x"),
            },
        ],
        refreshed: Some(1_760_000_000_000),
    }
}

#[test]
fn tables_round_trip_exactly_through_the_cache_parquet() {
    let t = table();
    let meta = Meta::new(&t, &model(&t), provenance()).unwrap();
    assert_eq!(
        meta.columns
            .iter()
            .map(|(_, t)| t.name())
            .collect::<Vec<_>>(),
        [
            "text",
            "whole",
            "decimal",
            "fixedDecimal",
            "fixedDecimal",
            "boolean"
        ]
    );
    let bytes = encode_table(&t).unwrap();
    assert_eq!(encode_table(&t).unwrap(), bytes, "deterministic");
    let back = decode_table(Bytes::from(bytes), &meta, usize::MAX).unwrap();
    assert_eq!(back, t);
}

#[test]
fn metadata_round_trips_with_paths_relative_to_the_project() {
    let t = table();
    let meta = Meta::new(&t, &model(&t), provenance()).unwrap();
    let root = &abs("/p");
    let text = String::from_utf8(meta.print(Some(root))).unwrap();
    assert!(text.contains("\"path\": \"data/Sales.csv\""), "{text}");
    assert!(text.contains("\"missing\": true"), "{text}");
    assert!(text.ends_with("}\n"));
    assert_eq!(Meta::parse(text.as_bytes(), Some(root)).unwrap(), meta);
    assert!(meta.fits(&model(&t)));
    let mut other = model(&t);
    other.columns.pop();
    assert!(!meta.fits(&other));
}

#[test]
fn hostile_metadata_and_parquet_are_errors_not_panics() {
    for bad in [
        &b""[..],
        b"[]",
        b"{\"formatVersion\": 2}",
        b"{\"formatVersion\": 1, \"table\": 3}",
        b"{\"formatVersion\": 1, \"table\": \"T\", \"rows\": 1, \"columns\": [{\"name\": \"a\", \
          \"type\": \"blob\"}], \"overrides\": [], \"sources\": [], \"m\": \"x\"}",
    ] {
        assert!(Meta::parse(bad, None).is_err());
    }
    let t = table();
    let meta = Meta::new(&t, &model(&t), provenance()).unwrap();
    let good = encode_table(&t).unwrap();
    let mut flipped = good.clone();
    for i in (0..flipped.len()).step_by(7) {
        flipped[i] ^= 0x5a;
    }
    let truncated = good[..good.len() / 2].to_vec();
    for bad in [Vec::new(), b"PAR1junkPAR1".to_vec(), flipped, truncated] {
        assert!(decode_table(Bytes::from(bad), &meta, usize::MAX).is_err());
    }
    // Data that doesn't match its metadata.
    let mut wrong = meta.clone();
    wrong.rows = 4;
    assert!(decode_table(Bytes::from(good.clone()), &wrong, usize::MAX).is_err());
    // The memory cap applies.
    let e = decode_table(Bytes::from(good), &meta, 16).unwrap_err();
    assert!(e.contains("memory"), "{e}");
}

#[test]
fn the_m_hash_ignores_paths_and_covers_merged_queries() {
    let file = |p: &str| File::Path(PathBuf::from(p));
    let source = |p: &str| metriccraft_data::source::Source::Parquet { file: file(p) };
    let culture = Culture::get("en-US").unwrap();
    let q = |name: &str, p: &str| Query {
        name: name.into(),
        source: source(p),
        steps: Vec::new(),
        culture,
    };
    let a = vec![q("A", "/one/a.parquet"), q("B", "/one/b.parquet")];
    let moved = vec![q("A", "/two/a.parquet"), q("B", "/two/b.parquet")];
    assert_eq!(m_hash(&a, "A"), m_hash(&moved, "A"));
    let mut edited = a.clone();
    edited[0].steps.push(Step::PromotedHeaders);
    assert_ne!(m_hash(&a, "A"), m_hash(&edited, "A"));
    assert_eq!(closure(&a, "A").len(), 1);
}

#[test]
fn only_other_cache_files_are_removed() {
    let dir = std::env::temp_dir().join(format!("metriccraft-cache-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    write(&dir, "Keep", Some(b"p"), b"m").unwrap();
    write(&dir, "Gone", Some(b"p"), b"m").unwrap();
    fs::write(dir.join(".cache/notes.txt"), "mine").unwrap();
    fs::write(dir.join(".cache/Keep.meta.tmp"), "left over").unwrap();
    assert!(exists_in(&dir, "Keep") && exists_in(&dir, "Gone"));
    let keep: BTreeSet<String> = ["Keep".to_string()].into();
    remove_others(&dir, &keep).unwrap();
    assert!(exists_in(&dir, "Keep"));
    assert!(!exists_in(&dir, "Gone"));
    assert!(dir.join(".cache/notes.txt").exists());
    assert!(!dir.join(".cache/Keep.meta.tmp").exists());
    assert_eq!(read_meta(&dir, "Keep").unwrap().unwrap(), b"m");
    assert!(read_meta(&dir, "Gone").unwrap().is_none());
    let _ = fs::remove_dir_all(&dir);
}
