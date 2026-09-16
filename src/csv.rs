//! Reading and writing OHLCV bar files in CSV form.
//!
//! Files match `samples/sample-bars.csv`: a header line of
//! `timestamp,open,high,low,close,volume` followed by one row per bar.
//! Timestamps are Unix seconds; the other columns are decimal numbers.

use std::{fs, io::Write, path::Path, time::UNIX_EPOCH};

use crate::{error::Error, market::Bar};

/// The expected CSV header line. A different header is an error rather than a
/// silent schema mismatch.
pub const HEADER: &str = "timestamp,open,high,low,close,volume";

/// Parses `path` into bars and validates them in file order.
///
/// # Errors
///
/// Returns [`Error::MarketData`] if the file cannot be read, a line has the
/// wrong number of fields, or any field is not a valid finite number. Errors
/// carry the 1-based line number so callers can locate bad rows.
pub fn load_bars(path: impl AsRef<Path>) -> Result<Vec<Bar>, Error> {
    let path = path.as_ref();
    let text = fs::read_to_string(path).map_err(|err| {
        Error::MarketData(format!("cannot read bar file {}: {err}", path.display()))
    })?;

    let mut bars = Vec::new();
    for (idx, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let line_no = i32::try_from(idx).unwrap_or(i32::MAX - 1).saturating_add(1);
        // Line 0 must be the header; from line 1 on every row is data.
        if idx == 0 {
            if line.trim() != HEADER {
                return Err(Error::MarketData(format!(
                    "line 1: unexpected CSV header `{}` (expected `{HEADER}`)",
                    line.trim()
                )));
            }
            continue;
        }
        let fields: Vec<&str> = line.split(',').collect();
        match fields.as_slice() {
            [ts, op, hi, lo, cl, vol] => bars.push(parse_bar(line_no, ts, op, hi, lo, cl, vol)?),
            other => {
                return Err(Error::MarketData(format!(
                    "line {line_no}: expected 6 comma-separated fields, found {}",
                    other.len()
                )))
            }
        }
    }

    if bars.is_empty() {
        return Err(Error::MarketData(
            "bar file contains no data lines".to_string(),
        ));
    }
    Ok(bars)
}

/// Writes `bars` to `path`, prefixing the standard header.
///
/// # Errors
///
/// Returns [`Error::MarketData`] on create, write or flush failure.
pub fn save_bars(bars: impl IntoIterator<Item = Bar>, path: impl AsRef<Path>) -> Result<(), Error> {
    let path = path.as_ref();
    let write_failed = || Error::MarketData(format!("cannot write to {}", path.display()));
    let mut out = fs::File::create(path)
        .map_err(|err| Error::MarketData(format!("cannot create {}: {err}", path.display())))?;

    writeln!(out, "{HEADER}").map_err(|_| write_failed())?;
    for bar in bars {
        // `{}` (Display) is a lossless round-trip representation for any
        // finite f64 — arbitrary-precision prices survive save→load exactly,
        // unlike fixed `"{:.2}"` formatting.
        writeln!(
            out,
            "{},{},{},{},{},{}",
            unix_secs(bar.timestamp()),
            bar.open(),
            bar.high(),
            bar.low(),
            bar.close(),
            bar.volume()
        )
        .map_err(|_| write_failed())?;
    }
    out.flush().map_err(|_| write_failed())?;
    Ok(())
}

fn bad(idx: i32, name: &str, value: &str) -> Error {
    Error::MarketData(format!(
        "line {idx}: invalid {name} value `{value}` (expected a finite number)"
    ))
}

fn parse_num(field: &str, idx: i32, name: &str) -> Result<f64, Error> {
    let value = field
        .trim()
        .parse::<f64>()
        .map_err(|_| bad(idx, name, field))?;
    if !value.is_finite() {
        return Err(bad(idx, name, field));
    }
    Ok(value)
}

fn parse_bar(
    idx: i32,
    ts: &str,
    op: &str,
    hi: &str,
    lo: &str,
    cl: &str,
    vol: &str,
) -> Result<Bar, Error> {
    let secs = ts
        .trim()
        .parse::<u64>()
        .map_err(|_| bad(idx, "timestamp", ts))?;
    let bar_ts = UNIX_EPOCH
        .checked_add(std::time::Duration::from_secs(secs))
        .ok_or_else(|| bad(idx, "timestamp", ts))?;
    Bar::new(
        bar_ts,
        parse_num(op, idx, "open")?,
        parse_num(hi, idx, "high")?,
        parse_num(lo, idx, "low")?,
        parse_num(cl, idx, "close")?,
        parse_num(vol, idx, "volume")?,
    )
}

#[allow(clippy::arithmetic_side_effects)]
fn unix_secs(ts: std::time::SystemTime) -> i64 {
    ts.duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    fn testdir() -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        let dir = std::env::temp_dir().join(format!("pa-csv-test-{nanos}"));
        std::fs::create_dir_all(&dir).expect("testdir");
        dir
    }

    #[allow(clippy::arithmetic_side_effects)] // bounded test offsets
    fn sample_bars() -> Vec<Bar> {
        vec![
            Bar::new(UNIX_EPOCH, 1.0, 2.0, 0.5, 10.0, 5.0).expect("valid"),
            Bar::new(
                UNIX_EPOCH + Duration::from_secs(1),
                2.0,
                3.5,
                1.5,
                22.0,
                7.0,
            )
            .expect("valid"),
            Bar::new(
                UNIX_EPOCH + Duration::from_secs(42),
                3.0,
                4.0,
                2.5,
                35.0,
                9.0,
            )
            .expect("valid"),
        ]
    }

    #[test]
    fn load_round_trips_bars() {
        let dir = testdir();
        let path = dir.join("bars.csv");
        save_bars(sample_bars(), &path).expect("save");

        let loaded = load_bars(&path).expect("load");
        assert_eq!(loaded.len(), 3);
        // Epsilon float equality: strict f64 comparison is lint-denied and the
        // round-trip is lossless at two decimals in any case.
        assert!((loaded[0].close() - 10.0).abs() < f64::EPSILON * 32.0);
        assert_eq!(
            unix_secs(loaded[2].timestamp()),
            "42".parse::<i64>().unwrap_or(0)
        );
    }

    #[test]
    fn load_rejects_bad_header() {
        let dir = testdir();
        let path = dir.join("bad.csv");
        fs::write(&path, "date,o,h,l,c\n").expect("write");
        let err = load_bars(&path).expect_err("bad header");
        assert!(err.to_string().contains("unexpected CSV header"));
    }

    #[test]
    fn load_rejects_short_rows() {
        let dir = testdir();
        let path = dir.join("short.csv");
        fs::write(&path, "timestamp,open,high,low,close,volume\n1,2,3\n").expect("write");
        let err = load_bars(&path).expect_err("short row");
        assert!(err
            .to_string()
            .contains("expected 6 comma-separated fields"));
    }

    #[test]
    fn load_rejects_non_numeric() {
        let dir = testdir();
        let path = dir.join("nonnum.csv");
        fs::write(
            &path,
            "timestamp,open,high,low,close,volume\n1,oops,2,3,4,5\n",
        )
        .expect("write");
        let err = load_bars(&path).expect_err("non-numeric");
        assert!(err.to_string().contains("invalid open value"));
    }

    #[test]
    fn load_rejects_infinite_price() {
        // Rust's f64 FromStr accepts "inf", so the finiteness check matters.
        let dir = testdir();
        let path = dir.join("inf.csv");
        fs::write(
            &path,
            "timestamp,open,high,low,close,volume\n1,1,1,inf,1,1\n",
        )
        .expect("write");
        let err = load_bars(&path).expect_err("infinite");
        assert!(err.to_string().contains("invalid low value"));
    }

    #[test]
    fn load_rejects_empty_data() {
        let dir = testdir();
        let path = dir.join("empty.csv");
        fs::write(&path, HEADER).expect("write");
        let err = load_bars(&path).expect_err("no data lines");
        assert!(err.to_string().contains("no data lines"));
    }

    #[test]
    fn load_rejects_missing_file() {
        let dir = testdir();
        let path = dir.join("missing.csv");
        let err = load_bars(&path).expect_err("missing file");
        assert!(err.to_string().contains("cannot read bar file"));
    }

    #[test]
    fn save_round_trips_prices_with_more_than_two_decimals() {
        // Values parsed from strings so the literals never look "unreadable";
        // parsing is bit-exact, which is what the assertions below rely on.
        let p = |s: &str| -> f64 { s.parse().expect("parse") };
        // Round-trip via Display must be lossless (bit-for-bit), including
        // values a fixed {:.2} format would silently truncate. Replay is then
        // exercised end-to-end over the loaded bars to prove behaviour holds.
        let dir = testdir();
        let path = dir.join("precise.csv");
        let original: Vec<Bar> = [
            Bar::new(
                UNIX_EPOCH,
                p("100.23456789"),
                p("100.987654321"),
                p("100.12345678"),
                p("100.5"),
                12.34,
            ),
            Bar::new(
                UNIX_EPOCH + Duration::from_secs(60),
                p("100.50000001"),
                p("101.987654321"),
                100.5,
                101.5,
                p("8.77"),
            ),
            Bar::new(
                UNIX_EPOCH + Duration::from_secs(120),
                p("101.50000001"),
                102.9,
                p("101.11111111"),
                102.22,
                3.6,
            ),
            Bar::new(
                UNIX_EPOCH + Duration::from_secs(180),
                p("102.22000001"),
                p("103.45678901"),
                101.7,
                103.05,
                22.5,
            ),
        ]
        .into_iter()
        .collect::<Result<Vec<_>, Error>>()
        .expect("valid bars");

        save_bars(original.clone(), &path).expect("save");
        let loaded = load_bars(&path).expect("load");
        assert_eq!(loaded.len(), original.len());
        for (i, (want, got)) in std::iter::zip(original.iter(), loaded.iter()).enumerate() {
            // Display is a bit-exact round-trip for any finite f64: this
            // catches both truncation and re-rounding.
            assert_eq!(got.open().to_bits(), want.open().to_bits(), "bar {i}");
            assert_eq!(got.high().to_bits(), want.high().to_bits(), "bar {i}");
            assert_eq!(got.low().to_bits(), want.low().to_bits(), "bar {i}");
            assert_eq!(got.close().to_bits(), want.close().to_bits(), "bar {i}");
            assert_eq!(got.volume().to_bits(), want.volume().to_bits(), "bar {i}");
        }

        // Replay behaviour survives the round-trip: four rising closes with
        // the default threshold-3 strategy → one Long entry, as before.
        let config = crate::config::Config::default();
        let report = crate::replay::run(&config, &path).expect("replay run");
        assert_eq!(report.bars, original.len());
        assert_eq!(report.entries, 1);
        assert!(matches!(report.final_signal, crate::strategy::Signal::Long));
    }

    #[test]
    fn unix_secs_helper_handles_epoch_and_offsets() {
        assert_eq!(unix_secs(SystemTime::UNIX_EPOCH), 0);
        assert_eq!(
            unix_secs(SystemTime::UNIX_EPOCH + Duration::from_secs(9)),
            9
        );
    }
}
