//! The reference dataset (spec N-2, N-8; `docs/bench.md`): a deterministic sales fact table of
//! 20 columns, written as Parquet (and CSV or XLSX) by `cargo xtask dataset` and by the import
//! benches. Same rows on every machine: a fixed seed and a splitmix64 stream, no RNG crate.
//!
//! | Column       | Type     | Values                                                |
//! |--------------|----------|-------------------------------------------------------|
//! | OrderID      | whole    | 1..rows, unique                                       |
//! | OrderNumber  | text     | `SO-00000001`…, unique                                |
//! | OrderDate    | date     | 2019-01-01 .. 2025-12-30                              |
//! | ShipDateTime | datetime | order date + 0–7 days, any second; 3% null            |
//! | CustomerID   | whole    | 1..100,000                                            |
//! | CustomerName | text     | `Customer 000042`, one per CustomerID (100,000)       |
//! | Region       | text     | 5, from Country                                       |
//! | Country      | text     | 20, from City                                         |
//! | City         | text     | 1,000                                                 |
//! | Product      | text     | 5,000                                                 |
//! | Category     | text     | 12, from Product                                      |
//! | Channel      | text     | 4, weighted                                           |
//! | Status       | text     | 6, weighted; 1% null                                  |
//! | Quantity     | whole    | 1..19, skewed low                                     |
//! | UnitPrice    | decimal  | 1.00..500.00, one per Product                         |
//! | Discount     | decimal  | 0, 0.05 .. 0.2; 20% null                              |
//! | SalesAmount  | decimal  | Quantity × UnitPrice × (1 − Discount), cents          |
//! | Cost         | decimal  | 40–80% of SalesAmount, cents                          |
//! | IsReturned   | boolean  | 4% true                                               |
//! | PromoCode    | text     | `PROMO-042`, 200 codes; 85% null                      |

use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use arrow_array::{
    ArrayRef, BooleanArray, Date32Array, Float64Array, Int64Array, RecordBatch, StringArray,
    TimestampMicrosecondArray,
};
use parquet::arrow::ArrowWriter;
use parquet::basic::Compression;
use parquet::file::properties::WriterProperties;

use crate::Result;

/// Rows in the reference dataset (spec N-2).
pub const REFERENCE_ROWS: usize = 10_000_000;
/// Bump when the generated rows change, so cached bench files are regenerated.
pub const VERSION: u32 = 1;
const SEED: u64 = 0x000D_45B1_2026_0009;
/// Rows generated (and written) at a time: bounds the generator's own memory.
const CHUNK: usize = 1 << 16;
/// Rows per Parquet row group (the writers' common default).
const ROW_GROUP: usize = 1 << 20;

/// Days from 1970-01-01 to 2019-01-01.
const FIRST_DAY: i64 = 17_897;
const DAY_US: i64 = 86_400_000_000;

const DISCOUNTS: [f64; 5] = [0.0, 0.05, 0.1, 0.15, 0.2];
const REGIONS: [&str; 5] = ["North", "South", "East", "West", "Central"];
const COUNTRIES: [&str; 20] = [
    "Norway",
    "Sweden",
    "Finland",
    "Denmark",
    "Spain",
    "Portugal",
    "Italy",
    "Greece",
    "Poland",
    "Hungary",
    "Romania",
    "Bulgaria",
    "Ireland",
    "France",
    "Belgium",
    "Netherlands",
    "Germany",
    "Austria",
    "Switzerland",
    "Czechia",
];
const CATEGORIES: [&str; 12] = [
    "Bikes",
    "Helmets",
    "Clothing",
    "Shoes",
    "Tents",
    "Camping",
    "Climbing",
    "Water Sports",
    "Fitness",
    "Garden",
    "Tools",
    "Accessories",
];
const CHANNELS: [(&str, u64); 4] = [("Online", 50), ("Store", 30), ("Partner", 15), ("Phone", 5)];
const STATUSES: [(&str, u64); 6] = [
    ("Delivered", 70),
    ("Shipped", 12),
    ("Processing", 8),
    ("Returned", 4),
    ("Cancelled", 4),
    ("On Hold", 2),
];

const COLUMNS: [&str; 20] = [
    "OrderID",
    "OrderNumber",
    "OrderDate",
    "ShipDateTime",
    "CustomerID",
    "CustomerName",
    "Region",
    "Country",
    "City",
    "Product",
    "Category",
    "Channel",
    "Status",
    "Quantity",
    "UnitPrice",
    "Discount",
    "SalesAmount",
    "Cost",
    "IsReturned",
    "PromoCode",
];

/// splitmix64.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        mix(self.0)
    }

    /// `0..n` (the modulo bias is irrelevant here).
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }

    fn chance(&mut self, percent: u64) -> bool {
        self.below(100) < percent
    }

    fn pick<'a>(&mut self, weighted: &[(&'a str, u64)]) -> &'a str {
        let total: u64 = weighted.iter().map(|(_, w)| w).sum();
        let mut r = self.below(total);
        for (v, w) in weighted {
            if r < *w {
                return v;
            }
            r -= w;
        }
        ""
    }
}

fn mix(mut z: u64) -> u64 {
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// One row; ids are 1-based.
struct Row {
    order: i64,
    date: i32,
    ship: Option<i64>,
    customer: i64,
    city: u64,
    product: u64,
    channel: &'static str,
    status: Option<&'static str>,
    quantity: i64,
    price: f64,
    discount: Option<f64>,
    amount: f64,
    cost: f64,
    returned: bool,
    promo: Option<u64>,
}

impl Row {
    fn country(&self) -> usize {
        (self.city % 20) as usize
    }
    fn region(&self) -> &'static str {
        REGIONS.get(self.country() / 4).copied().unwrap_or_default()
    }
}

fn cents(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

/// The rows `start..start + n`; `rng` carries on from the previous chunk.
fn rows(rng: &mut Rng, start: usize, n: usize) -> Vec<Row> {
    (start..start + n)
        .map(|i| {
            let date = FIRST_DAY + rng.below(7 * 365) as i64;
            let ship =
                (!rng.chance(3)).then(|| date * DAY_US + rng.below(8 * 86_400) as i64 * 1_000_000);
            let product = 1 + rng.below(5_000);
            let price = cents(1.0 + (mix(product) % 49_900) as f64 / 100.0);
            let quantity = 1 + (rng.below(20) * rng.below(20)) as i64 / 20;
            let discount = (!rng.chance(20))
                .then(|| DISCOUNTS.get(rng.below(5) as usize).copied().unwrap_or(0.0));
            let amount = cents(quantity as f64 * price * (1.0 - discount.unwrap_or(0.0)));
            Row {
                order: i as i64 + 1,
                date: date as i32,
                ship,
                customer: 1 + rng.below(100_000) as i64,
                city: 1 + rng.below(1_000),
                product,
                channel: rng.pick(&CHANNELS),
                status: (!rng.chance(1)).then(|| rng.pick(&STATUSES)),
                quantity,
                price,
                discount,
                amount,
                cost: cents(amount * (40 + rng.below(41)) as f64 / 100.0),
                returned: rng.chance(4),
                promo: rng.chance(15).then(|| rng.below(200)),
            }
        })
        .collect()
}

/// Calls `f` with each chunk of the dataset's first `total` rows, in order.
fn chunks(total: usize, mut f: impl FnMut(&[Row]) -> Result) -> Result {
    let mut rng = Rng(SEED);
    let mut start = 0;
    while start < total {
        let n = CHUNK.min(total - start);
        f(&rows(&mut rng, start, n))?;
        start += n;
    }
    Ok(())
}

fn batch(rows: &[Row]) -> Result<RecordBatch> {
    let text = |f: &dyn Fn(&Row) -> Option<String>| -> ArrayRef {
        Arc::new(rows.iter().map(f).collect::<StringArray>())
    };
    let columns: Vec<ArrayRef> = vec![
        Arc::new(Int64Array::from_iter_values(rows.iter().map(|r| r.order))),
        text(&|r| Some(format!("SO-{:08}", r.order))),
        Arc::new(Date32Array::from_iter_values(rows.iter().map(|r| r.date))),
        Arc::new(TimestampMicrosecondArray::from_iter(
            rows.iter().map(|r| r.ship),
        )),
        Arc::new(Int64Array::from_iter_values(
            rows.iter().map(|r| r.customer),
        )),
        text(&|r| Some(format!("Customer {:06}", r.customer))),
        text(&|r| Some(r.region().into())),
        text(&|r| COUNTRIES.get(r.country()).map(|c| (*c).into())),
        text(&|r| Some(format!("City {:04}", r.city))),
        text(&|r| Some(format!("Product {:04}", r.product))),
        text(&|r| {
            CATEGORIES
                .get((r.product % 12) as usize)
                .map(|c| (*c).into())
        }),
        text(&|r| Some(r.channel.into())),
        text(&|r| r.status.map(Into::into)),
        Arc::new(Int64Array::from_iter_values(
            rows.iter().map(|r| r.quantity),
        )),
        Arc::new(Float64Array::from_iter_values(rows.iter().map(|r| r.price))),
        Arc::new(Float64Array::from_iter(rows.iter().map(|r| r.discount))),
        Arc::new(Float64Array::from_iter_values(
            rows.iter().map(|r| r.amount),
        )),
        Arc::new(Float64Array::from_iter_values(rows.iter().map(|r| r.cost))),
        Arc::new(BooleanArray::from_iter(
            rows.iter().map(|r| Some(r.returned)),
        )),
        text(&|r| r.promo.map(|p| format!("PROMO-{p:03}"))),
    ];
    RecordBatch::try_from_iter(COLUMNS.iter().zip(columns)).map_err(|e| e.to_string())
}

/// Writes the first `rows` rows as Parquet (snappy, 1M-row row groups).
pub fn write_parquet(path: &Path, rows: usize) -> Result {
    let file = std::fs::File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let props = WriterProperties::builder()
        .set_compression(Compression::SNAPPY)
        .set_max_row_group_row_count(Some(ROW_GROUP))
        .build();
    let mut writer: Option<ArrowWriter<std::fs::File>> = None;
    let mut file = Some(file);
    chunks(rows, |chunk| {
        let b = batch(chunk)?;
        if writer.is_none() {
            let f = file.take().ok_or("parquet writer")?;
            let w = ArrowWriter::try_new(f, b.schema(), Some(props.clone()))
                .map_err(|e| e.to_string())?;
            writer = Some(w);
        }
        writer
            .as_mut()
            .ok_or("parquet writer")?
            .write(&b)
            .map_err(|e| e.to_string())
    })?;
    writer
        .ok_or("no rows to write")?
        .close()
        .map(drop)
        .map_err(|e| e.to_string())
}

/// `days` since 1970-01-01 as (year, month, day) (Hinnant's civil-from-days).
fn civil(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// Writes the first `rows` rows as UTF-8 CSV: a header row, ISO dates, `.` decimals, `true` /
/// `false`, empty for null.
pub fn write_csv(path: &Path, rows: usize) -> Result {
    let file = std::fs::File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut w = BufWriter::new(file);
    let io = |e: std::io::Error| format!("{}: {e}", path.display());
    writeln!(w, "{}", COLUMNS.join(",")).map_err(io)?;
    chunks(rows, |chunk| {
        for r in chunk {
            let (y, m, d) = civil(r.date.into());
            write!(w, "{},SO-{:08},{y:04}-{m:02}-{d:02},", r.order, r.order).map_err(io)?;
            if let Some(us) = r.ship {
                let (y, m, d) = civil(us.div_euclid(DAY_US));
                let s = us.rem_euclid(DAY_US) / 1_000_000;
                let (hh, mm, ss) = (s / 3600, s / 60 % 60, s % 60);
                write!(w, "{y:04}-{m:02}-{d:02} {hh:02}:{mm:02}:{ss:02}").map_err(io)?;
            }
            writeln!(
                w,
                ",{c},Customer {c:06},{},{},City {:04},Product {:04},{},{},{},{},{},{},{},{},{},{}",
                r.region(),
                COUNTRIES.get(r.country()).unwrap_or(&""),
                r.city,
                r.product,
                CATEGORIES.get((r.product % 12) as usize).unwrap_or(&""),
                r.channel,
                r.status.unwrap_or_default(),
                r.quantity,
                r.price,
                r.discount.map(|d| d.to_string()).unwrap_or_default(),
                r.amount,
                r.cost,
                r.returned,
                r.promo.map(|p| format!("PROMO-{p:03}")).unwrap_or_default(),
                c = r.customer,
            )
            .map_err(io)?;
        }
        Ok(())
    })?;
    w.flush().map_err(io)
}

/// Writes the first `rows` rows as one sheet, `Sales`, with a header row (dates as Excel dates).
pub fn write_xlsx(path: &Path, rows: usize) -> Result {
    use rust_xlsxwriter::{Format, Workbook};
    let x = |e: rust_xlsxwriter::XlsxError| e.to_string();
    let mut book = Workbook::new();
    let sheet = book.add_worksheet().set_name("Sales").map_err(x)?;
    let date = Format::new().set_num_format("yyyy-mm-dd");
    let datetime = Format::new().set_num_format("yyyy-mm-dd hh:mm:ss");
    for (c, name) in (0u16..).zip(COLUMNS) {
        sheet.write_string(0, c, name).map_err(x)?;
    }
    // Excel's serial day 25,569 is 1970-01-01.
    let excel = |days: f64| days + 25_569.0;
    let mut row = 0u32;
    chunks(rows, |chunk| {
        for r in chunk {
            row += 1;
            let texts = [
                (1, Some(format!("SO-{:08}", r.order))),
                (5, Some(format!("Customer {:06}", r.customer))),
                (6, Some(r.region().into())),
                (7, COUNTRIES.get(r.country()).map(|c| (*c).into())),
                (8, Some(format!("City {:04}", r.city))),
                (9, Some(format!("Product {:04}", r.product))),
                (
                    10,
                    CATEGORIES
                        .get((r.product % 12) as usize)
                        .map(|c| (*c).into()),
                ),
                (11, Some(r.channel.into())),
                (12, r.status.map(Into::into)),
                (19, r.promo.map(|p| format!("PROMO-{p:03}"))),
            ];
            for (c, v) in texts {
                if let Some(v) = v {
                    sheet.write_string(row, c, v).map_err(x)?;
                }
            }
            let numbers = [
                (0, Some(r.order as f64)),
                (4, Some(r.customer as f64)),
                (13, Some(r.quantity as f64)),
                (14, Some(r.price)),
                (15, r.discount),
                (16, Some(r.amount)),
                (17, Some(r.cost)),
            ];
            for (c, v) in numbers {
                if let Some(v) = v {
                    sheet.write_number(row, c, v).map_err(x)?;
                }
            }
            sheet
                .write_number_with_format(row, 2, excel(f64::from(r.date)), &date)
                .map_err(x)?;
            if let Some(us) = r.ship {
                let days = us as f64 / DAY_US as f64;
                sheet
                    .write_number_with_format(row, 3, excel(days), &datetime)
                    .map_err(x)?;
            }
            sheet.write_boolean(row, 18, r.returned).map_err(x)?;
        }
        Ok(())
    })?;
    book.save(path).map_err(x)
}

/// `10m`, `200k`, or the number.
pub fn label(rows: usize) -> String {
    match rows {
        r if r >= 1_000_000 && r % 1_000_000 == 0 => format!("{}m", r / 1_000_000),
        r if r >= 1_000 && r % 1_000 == 0 => format!("{}k", r / 1_000),
        r => r.to_string(),
    }
}

/// `dir/sales_<label>.<ext>`, written by `write` unless it is there already.
pub fn cached(
    dir: &Path,
    ext: &str,
    rows: usize,
    write: fn(&Path, usize) -> Result,
) -> Result<PathBuf> {
    std::fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    let path = dir.join(format!("sales_{}.{ext}", label(rows)));
    if !path.exists() {
        eprintln!("  generating {}", path.display());
        let tmp = path.with_extension(format!("{ext}.tmp"));
        write(&tmp, rows)?;
        std::fs::rename(&tmp, &path).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok(path)
}

/// `cargo xtask dataset [--rows N] [--out DIR] [--csv]`.
pub fn run(args: &[String]) -> Result {
    let value = |flag: &str| {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1))
    };
    let rows = match value("--rows") {
        Some(n) => n
            .parse()
            .map_err(|_| format!("--rows: `{n}` is not a count"))?,
        None => REFERENCE_ROWS,
    };
    let out = PathBuf::from(value("--out").map_or("observations/reference", String::as_str));
    let mut written = vec![cached(&out, "parquet", rows, write_parquet)?];
    if args.iter().any(|a| a == "--csv") {
        written.push(cached(&out, "csv", rows, write_csv)?);
    }
    for path in written {
        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        eprintln!("  {} ({} MB)", path.display(), size >> 20);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_are_deterministic_and_chunk_independent() {
        let mut a = Vec::new();
        chunks(CHUNK + 10, |c| {
            a.extend(c.iter().map(|r| (r.order, r.amount.to_bits(), r.promo)));
            Ok(())
        })
        .unwrap();
        let mut b = Vec::new();
        chunks(CHUNK + 10, |c| {
            b.extend(c.iter().map(|r| (r.order, r.amount.to_bits(), r.promo)));
            Ok(())
        })
        .unwrap();
        assert_eq!(a, b);
        assert_eq!(a.len(), CHUNK + 10);
        assert_eq!(a.last().map(|r| r.0), Some(CHUNK as i64 + 10));
    }

    #[test]
    fn batches_have_twenty_columns() {
        let mut rng = Rng(SEED);
        let b = batch(&rows(&mut rng, 0, 100)).unwrap();
        assert_eq!((b.num_columns(), b.num_rows()), (20, 100));
    }

    #[test]
    fn civil_dates() {
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(FIRST_DAY), (2019, 1, 1));
        assert_eq!(civil(19_782), (2024, 2, 29));
    }

    #[test]
    fn labels() {
        assert_eq!(label(10_000_000), "10m");
        assert_eq!(label(200_000), "200k");
        assert_eq!(label(1_234), "1234");
    }
}
