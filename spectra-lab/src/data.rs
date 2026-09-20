//! CSV ingestion and independent input diagnostics.
//!
//! Diagnostics are reported separately so that one problem never masks
//! another:
//! - `nonfinite_rows`: rows dropped because x or y is NaN/inf;
//! - `duplicate_x`: distinct repeated x coordinates (fitting is still
//!   allowed, but the rows are flagged);
//! - `nonmonotonic_pairs`: adjacent rows that break monotonic ordering;
//! - `too_few_rows`: fewer than 8 finite rows => fitting/refusal;
//! - parse errors per row.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const LOADER_VERSION: &str = "csv-loader-v1.0.0";
pub const MIN_ROWS: usize = 8;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Diagnostics {
    pub rows_read: usize,
    pub rows_kept: usize,
    pub parse_errors: Vec<(usize, String)>,
    pub nonfinite_rows: Vec<usize>,
    pub duplicate_x: Vec<f64>,
    pub nonmonotonic_pairs: Vec<(usize, f64, f64)>,
    pub monotonic: Option<String>, // "increasing" | "decreasing" | null
    pub too_few_rows: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Spectrum {
    pub x: Vec<f64>,
    pub y: Vec<f64>,
    /// original 1-based CSV row number for each kept point.
    pub source_rows: Vec<usize>,
    pub diagnostics: Diagnostics,
    pub x_header: String,
    pub y_header: String,
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    format!("{:x}", h.finalize())
}

fn split_fields(line: &str) -> Vec<String> {
    line.split(|c| c == ',' || c == ';' || c == '\t')
        .map(|s| s.trim().trim_matches('"').to_string())
        .collect()
}

pub fn parse_csv(text: &str) -> Result<Spectrum, String> {
    let mut lines = text.lines().filter(|l| {
        let t = l.trim();
        !t.is_empty() && !t.starts_with('#')
    });
    let header = lines.next().ok_or("empty CSV")?;
    let hf = split_fields(header);
    if hf.len() < 2 {
        return Err("header must contain at least two columns".into());
    }
    let x_header = hf[0].clone();
    let y_header = hf[1].clone();

    let mut x = Vec::new();
    let mut y = Vec::new();
    let mut source_rows = Vec::new();
    let mut parse_errors = Vec::new();
    let mut nonfinite_rows = Vec::new();
    let mut rows_read = 0usize;

    for (idx0, line) in lines.enumerate() {
        let row_no = idx0 + 2; // header is line 1
        rows_read += 1;
        let f = split_fields(line);
        if f.len() < 2 {
            parse_errors.push((row_no, "expected 2 numeric columns".into()));
            continue;
        }
        let xv = match f[0].parse::<f64>() {
            Ok(v) => v,
            Err(e) => {
                parse_errors.push((row_no, format!("x parse error: {e}")));
                continue;
            }
        };
        let yv = match f[1].parse::<f64>() {
            Ok(v) => v,
            Err(e) => {
                parse_errors.push((row_no, format!("y parse error: {e}")));
                continue;
            }
        };
        if !xv.is_finite() || !yv.is_finite() {
            nonfinite_rows.push(row_no);
            continue;
        }
        x.push(xv);
        y.push(yv);
        source_rows.push(row_no);
    }

    let mut sorted_x = x.clone();
    sorted_x.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mut duplicate_x = Vec::new();
    for w in sorted_x.windows(2) {
        if w[0] == w[1]
            && duplicate_x
                .last()
                .map(|v| *v != w[0])
                .unwrap_or(true)
        {
            duplicate_x.push(w[0]);
        }
    }

    let mut inc = true;
    let mut dec = true;
    let mut nonmonotonic_pairs = Vec::new();
    for i in 1..x.len() {
        if x[i] > x[i - 1] {
            dec = false;
        } else if x[i] < x[i - 1] {
            inc = false;
            nonmonotonic_pairs.push((i, x[i - 1], x[i]));
        } // equal does not break a monotone (non-strict) sequence
    }
    let monotonic = if x.len() < 2 {
        None
    } else if inc {
        Some("increasing".to_string())
    } else if dec {
        Some("decreasing".to_string())
    } else {
        None
    };

    let too_few_rows = x.len() < MIN_ROWS;
    Ok(Spectrum {
        diagnostics: Diagnostics {
            rows_read,
            rows_kept: x.len(),
            parse_errors,
            nonfinite_rows,
            duplicate_x,
            nonmonotonic_pairs,
            monotonic,
            too_few_rows,
        },
        x,
        y,
        source_rows,
        x_header,
        y_header,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostics_independent() {
        let csv = "w,y\n1,1\n2,2\n2,3\n3,nan\n4,5\n3,4\n5,6";
        let s = parse_csv(csv).unwrap();
        assert_eq!(s.diagnostics.rows_read, 7);
        assert_eq!(s.diagnostics.rows_kept, 6);
        assert_eq!(s.diagnostics.nonfinite_rows, vec![5]);
        assert_eq!(s.diagnostics.duplicate_x, vec![2.0]);
        assert_eq!(s.diagnostics.nonmonotonic_pairs.len(), 1);
        assert!(s.diagnostics.too_few_rows);
    }

    #[test]
    fn decreasing_is_monotonic() {
        let s = parse_csv("w,y\n5,1\n4,1\n3,1\n2,1\n1,1\n0,1\n-1,1\n-2,1").unwrap();
        assert_eq!(s.diagnostics.monotonic.as_deref(), Some("decreasing"));
        assert!(!s.diagnostics.too_few_rows);
    }

    #[test]
    fn header_units_detected() {
        let s = parse_csv("energy(eV),absorbance\n1,1\n2,2\n3,3\n4,4\n5,5\n6,6\n7,7\n8,8").unwrap();
        assert!(crate::units::XUnit::parse(&s.x_header).is_some());
    }
}
