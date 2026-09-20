//! Scheme specifications, content fingerprints and fit orchestration.

use crate::data::sha256_hex;
use crate::fit::{fit, FitReport, FITTER_VERSION};
use crate::model::{gaussian_smooth, poly_baseline, MODEL_VERSION};
use crate::model::PeakSpec;
use serde::{Deserialize, Serialize};

pub const SCHEME_VERSION: &str = "scheme-v1.0.0";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BaselineSpec {
    pub degree: usize,
    /// raw-unit anchor windows [lo, hi]
    pub windows: Vec<[f64; 2]>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchemeSpec {
    /// Gaussian smoothing sigma in raw x units (0 disables smoothing).
    #[serde(default)]
    pub smooth_sigma: f64,
    pub baseline: BaselineSpec,
    /// raw x coordinates of cosmic rays / user-excluded points.
    #[serde(default)]
    pub excluded_x: Vec<f64>,
    pub peaks: Vec<PeakSpec>,
}

/// Canonical JSON: re-serialised with sorted keys (serde_json BTreeMap style).
pub fn canonical_json<T: Serialize>(v: &T) -> String {
    let val = serde_json::to_value(v).expect("serialize");
    canonical_value(&val)
}

fn canonical_value(v: &serde_json::Value) -> String {
    serde_json::to_string(&sort_obj(v)).unwrap()
}

fn sort_obj(v: &serde_json::Value) -> serde_json::Value {
    match v {
        serde_json::Value::Object(m) => {
            let mut entries: Vec<(&String, &serde_json::Value)> = m.iter().collect();
            entries.sort_by(|a, b| a.0.cmp(b.0));
            let o: serde_json::Map<String, serde_json::Value> = entries
                .into_iter()
                .map(|(k, vv)| (k.clone(), sort_obj(vv)))
                .collect();
            serde_json::Value::Object(o)
        }
        serde_json::Value::Array(a) => {
            serde_json::Value::Array(a.iter().map(sort_obj).collect())
        }
        other => other.clone(),
    }
}

/// Deterministic fingerprint of (input, initials, constraints, versions).
pub fn fingerprint(
    input_hash: &str,
    spec: &SchemeSpec,
) -> String {
    let body = format!(
        "{}\n{}\n{}\n{}\n{}",
        input_hash,
        canonical_json(spec),
        MODEL_VERSION,
        FITTER_VERSION,
        SCHEME_VERSION
    );
    sha256_hex(body.as_bytes())
}

pub struct PreparedFit {
    pub mask: Vec<bool>,
    pub baseline: Vec<f64>,
    pub smoothed: Option<Vec<f64>>,
    pub report: FitReport,
    pub corrected: Vec<f64>,
}

const EXCLUSION_TOL: f64 = 1e-9;

fn exclusion_mask(xs: &[f64], excluded_x: &[f64]) -> Vec<bool> {
    let mut m = vec![false; xs.len()];
    for (i, x) in xs.iter().enumerate() {
        for ex in excluded_x {
            let scale = ex.abs().max(1.0);
            if (x - ex).abs() <= EXCLUSION_TOL * scale {
                m[i] = true;
            }
        }
    }
    m
}

/// Run the whole pipeline: mask -> baseline -> subtract -> smooth -> fit.
/// Smoothing is a display artifact; the fit always uses the unsmoothed,
/// baseline-corrected data.
pub fn run_pipeline(
    xs: &[f64],
    ys: &[f64],
    spec: &SchemeSpec,
) -> Result<PreparedFit, String> {
    let mask = exclusion_mask(xs, &spec.excluded_x);
    let windows: Vec<(f64, f64)> = spec
        .baseline
        .windows
        .iter()
        .map(|w| (w[0], w[1]))
        .collect();
    let baseline = poly_baseline(xs, ys, &mask, &windows, spec.baseline.degree)?;
    let corrected: Vec<f64> = ys.iter().zip(baseline.iter()).map(|(y, b)| y - b).collect();
    let smoothed = if spec.smooth_sigma > 0.0 {
        Some(gaussian_smooth(xs, &corrected, &mask, spec.smooth_sigma))
    } else {
        None
    };
    let report = fit(xs, &corrected, &mask, &spec.peaks);
    Ok(PreparedFit {
        mask,
        baseline,
        smoothed,
        report,
        corrected,
    })
}

/// Hash a derived curve and bind it to its input + method.
pub fn artifact_hash(
    input_hash: &str,
    upstream: Option<&str>,
    method_version: &str,
    params_canonical: &str,
    xs: &[f64],
    ys: &[f64],
) -> String {
    let mut body = format!(
        "{}\n{}\n{}\n{}\n",
        input_hash,
        upstream.unwrap_or("-"),
        method_version,
        params_canonical
    );
    for (x, y) in xs.iter().zip(ys.iter()) {
        use std::fmt::Write;
        let _ = write!(body, "{:.15e},{:.15e};", x, y);
    }
    sha256_hex(body.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_stable_and_sensitive() {
        let s = SchemeSpec {
            smooth_sigma: 0.0,
            baseline: BaselineSpec {
                degree: 1,
                windows: vec![[0.0, 1.0], [9.0, 10.0]],
            },
            excluded_x: vec![],
            peaks: vec![PeakSpec {
                kind: crate::model::PeakKind::Gauss,
                center0: 5.0,
                width0: 0.5,
                amp0: 1.0,
                fix_center: false,
            }],
        };
        let f1 = fingerprint("abc", &s);
        let f2 = fingerprint("abc", &s);
        assert_eq!(f1, f2);
        let mut s2 = s.clone();
        s2.peaks[0].fix_center = true;
        assert_ne!(f1, fingerprint("abc", &s2));
        assert_ne!(f1, fingerprint("abd", &s));
    }

    #[test]
    fn pipeline_excludes_and_baselines() {
        let xs: Vec<f64> = (0..200).map(|i| i as f64 * 0.05).collect();
        let ys: Vec<f64> = xs
            .iter()
            .map(|x| 0.3 + 0.01 * x + crate::model::gauss_line(*x, 5.0, 0.4, 2.0))
            .collect();
        let spec = SchemeSpec {
            smooth_sigma: 0.05,
            baseline: BaselineSpec {
                degree: 1,
                windows: vec![[0.0, 3.5], [6.5, 10.0]],
            },
            excluded_x: vec![7.7],
            peaks: vec![PeakSpec {
                kind: crate::model::PeakKind::Gauss,
                center0: 5.0,
                width0: 0.5,
                amp0: 1.5,
                fix_center: false,
            }],
        };
        let pf = run_pipeline(&xs, &ys, &spec).unwrap();
        assert!(pf.mask[154]); // 7.7 = index 154
        assert!(pf.report.converged);
        assert!(pf.smoothed.is_some());
    }
}
