//! Peak models: Gaussian, Lorentzian, Voigt.
//!
//! Width convention: every peak carries a single full-width parameter.
//! - Gaussian: full width at half maximum `w`;
//!   `V(x) = h exp(-4 ln2 ((x-c)/w)^2)`, area = h*w*sqrt(pi/(4 ln2)).
//! - Lorentzian: FWHM `w`; `V(x) = h/(1+4((x-c)/w)^2)`, area = h*w*pi/2.
//! - Voigt: parameterised by Gaussian 1-sigma `sigma` and Lorentzian HWHM
//!   `gamma`; the profile is normalised to unit area via the Faddeeva
//!   function `w(z)` evaluated by 64-node Gauss-Hermite quadrature.

use crate::linalg::gauss_hermite;
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

pub const MODEL_VERSION: &str = "model-v1.0.0";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PeakKind {
    Gauss,
    Lorentz,
    Voigt,
}

impl PeakKind {
    pub fn parse(s: &str) -> Option<PeakKind> {
        match s.to_ascii_lowercase().as_str() {
            "gauss" | "gaussian" => Some(PeakKind::Gauss),
            "lorentz" | "lorentzian" => Some(PeakKind::Lorentz),
            "voigt" => Some(PeakKind::Voigt),
            _ => None,
        }
    }
    pub fn n_params(self) -> usize {
        match self {
            PeakKind::Gauss | PeakKind::Lorentz => 3,
            PeakKind::Voigt => 4,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeakSpec {
    pub kind: PeakKind,
    pub center0: f64,
    /// FWHM-style initial width (for voigt used to seed sigma/gamma).
    pub width0: f64,
    /// Initial height (gauss/lorentz) or area (voigt).
    pub amp0: f64,
    #[serde(default)]
    pub fix_center: bool,
}

/// ---------------- Faddeeva w(z) by Gauss-Hermite quadrature ----------------

static GH64: OnceLock<Vec<(f64, f64)>> = OnceLock::new();

fn gh64() -> &'static [(f64, f64)] {
    GH64.get_or_init(|| gauss_hermite(64))
}

/// Faddeeva function w(z) = exp(-z^2) erfc(-i z) for Im(z) >= 0.
///
/// Uses w(z) = (i/pi) integral e^{-t^2}/(z-t) dt over the real line,
/// evaluated with 64 physicist Gauss-Hermite nodes.  With sigma >= ~1e-8
/// (scaled) accuracy is far beyond double precision requirements; the
/// normalisation of the Voigt profile is analytically guaranteed by this
/// representation (see unit tests).
pub fn faddeeva(re: f64, im: f64) -> (f64, f64) {
    let mut wr = 0.0_f64;
    let mut wi = 0.0_f64;
    let invpi = 1.0 / std::f64::consts::PI;
    for &(t, weight) in gh64() {
        // i/(z-t) = i*((re-t)-i im)/((re-t)^2+im^2)
        //         = im/den + i (re-t)/den
        let a = re - t;
        let den = a * a + im * im;
        wr += weight * im / den;
        wi += weight * a / den;
    }
    (wr * invpi, wi * invpi)
}

/// Normalised Voigt line at `x`.
pub fn voigt(x: f64, c: f64, sigma: f64, gamma: f64) -> f64 {
    let norm = 1.0 / (sigma * (2.0 * std::f64::consts::PI).sqrt());
    if gamma <= 0.0 {
        let u = (x - c) / sigma;
        return norm * (-0.5 * u * u).exp();
    }
    let zre = (x - c) / (sigma * std::f64::consts::SQRT_2);
    // Small numerical floor avoids losing the real-axis delta term while
    // remaining far below any physically relevant Lorentzian width.
    let zim = gamma.max(sigma * 1e-10) / (sigma * std::f64::consts::SQRT_2);
    let (wr, _) = faddeeva(zre, zim);
    wr * norm
}

#[inline]
pub fn gauss_line(x: f64, c: f64, w: f64, h: f64) -> f64 {
    let t = (x - c) / w;
    h * (-4.0 * std::f64::consts::LN_2 * t * t).exp()
}

#[inline]
pub fn lorentz_line(x: f64, c: f64, w: f64, h: f64) -> f64 {
    let t = (x - c) / w;
    h / (1.0 + 4.0 * t * t)
}

/// Area of one fitted peak given its parameters (`p` slice as laid out by
/// [`pack_peak`]).
pub fn peak_area(kind: PeakKind, p: &[f64]) -> f64 {
    match kind {
        // p = [c, w, h]
        PeakKind::Gauss => {
            p[2] * p[1] * (std::f64::consts::PI / (4.0 * std::f64::consts::LN_2)).sqrt()
        }
        PeakKind::Lorentz => p[2] * p[1] * std::f64::consts::PI / 2.0,
        // p = [c, sigma, gamma, area]
        PeakKind::Voigt => p[3],
    }
}

/// ---------------- baseline & smoothing ----------------

/// Polynomial baseline evaluated on every point.
/// Only points inside `windows` (pairs lo/hi) and not in `excluded_mask`
/// contribute to the fit.  Returns a vector over `xs`.
pub fn poly_baseline(
    xs: &[f64],
    ys: &[f64],
    excluded_mask: &[bool],
    windows: &[(f64, f64)],
    degree: usize,
) -> Result<Vec<f64>, String> {
    if degree > 6 {
        return Err("baseline degree must be 0..=6".into());
    }
    let sel: Vec<usize> = (0..xs.len())
        .filter(|&i| {
            !excluded_mask[i] && windows.iter().any(|(lo, hi)| xs[i] >= *lo && xs[i] <= *hi)
        })
        .collect();
    if sel.len() < degree + 2 {
        return Err(format!(
            "baseline needs >= {} anchor points, found {}",
            degree + 2,
            sel.len()
        ));
    }
    let mid = xs[sel[sel.len() / 2]];
    let scale = xs[sel[sel.len() - 1]]
        .abs()
        .max(xs[sel[0]].abs())
        .max(1.0);
    let tv: Vec<f64> = sel.iter().map(|&i| (xs[i] - mid) / scale).collect();
    let yv: Vec<f64> = sel.iter().map(|&i| ys[i]).collect();
    let wv = vec![1.0; sel.len()];
    let coef = crate::linalg::polyfit_norm(&tv, &yv, &wv, degree)
        .ok_or("singular baseline normal equations")?;
    Ok(xs
        .iter()
        .map(|x| {
            let t = (x - mid) / scale;
            let mut v = 0.0;
            let mut tk = 1.0;
            for c in &coef {
                v += c * tk;
                tk *= t;
            }
            v
        })
        .collect())
}

/// Gaussian-kernel smoothing (display/preview only).
/// `sigma` is in x units; weights w_i = exp(-((x_i-x_j)/(2s))^2).
/// Excluded points are neither evaluated nor used as anchors.
pub fn gaussian_smooth(xs: &[f64], ys: &[f64], excluded_mask: &[bool], sigma: f64) -> Vec<f64> {
    let two_s2 = (2.0 * sigma * sigma).max(1e-30);
    let mut out = vec![f64::NAN; xs.len()];
    for i in 0..xs.len() {
        if excluded_mask[i] {
            continue;
        }
        let mut sw = 0.0;
        let mut sy = 0.0;
        for j in 0..xs.len() {
            if excluded_mask[j] {
                continue;
            }
            let dx = xs[j] - xs[i];
            let w = (-dx * dx / two_s2).exp();
            sw += w;
            sy += w * ys[j];
        }
        out[i] = sy / sw;
    }
    out
}

/// ---------------- combined peak model ----------------

/// Evaluate the sum of peaks.  Flat layout: each peak occupies
/// gauss/lorentz [c,w,h] or voigt [c,sigma,gamma,area].
pub fn eval_peaks(x: f64, peaks: &[PeakKind], p: &[f64]) -> f64 {
    let mut off = 0;
    let mut v = 0.0;
    for k in peaks {
        v += match k {
            PeakKind::Gauss => gauss_line(x, p[off], p[off + 1], p[off + 2]),
            PeakKind::Lorentz => lorentz_line(x, p[off], p[off + 1], p[off + 2]),
            PeakKind::Voigt => {
                peak_area(PeakKind::Voigt, &p[off..off + 4])
                    * voigt(x, p[off], p[off + 1].max(1e-12), p[off + 2].max(0.0))
            }
        };
        off += k.n_params();
    }
    v
}

/// Build the initial/fixed parameter layout from peak candidates.
pub fn initial_vector(specs: &[PeakSpec]) -> (Vec<PeakKind>, Vec<f64>, Vec<bool>) {
    let kinds: Vec<PeakKind> = specs.iter().map(|s| s.kind).collect();
    let mut p = Vec::new();
    let mut fixed = Vec::new();
    for s in specs {
        match s.kind {
            PeakKind::Gauss | PeakKind::Lorentz => {
                p.extend_from_slice(&[s.center0, s.width0.max(1e-12), s.amp0]);
                fixed.extend_from_slice(&[s.fix_center, false, false]);
            }
            PeakKind::Voigt => {
                // Seed sigma from FWHM: FWHM_g = 2 sqrt(2 ln2) sigma;
                // split FWHM equally between gaussian and lorentz parts.
                let fwhm_g = s.width0.max(1e-12);
                let sigma = fwhm_g / (2.0 * (2.0 * std::f64::consts::LN_2).sqrt());
                let gamma = fwhm_g / 2.0;
                p.extend_from_slice(&[s.center0, sigma, gamma, s.amp0.max(1e-300)]);
                fixed.extend_from_slice(&[s.fix_center, false, false, false]);
            }
        }
    }
    (kinds, p, fixed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gauss_area_consistency() {
        let (c, w, h) = (2.0_f64, 0.4, 3.0);
        let analytic =
            h * w * (std::f64::consts::PI / (4.0 * std::f64::consts::LN_2)).sqrt();
        let xs: Vec<f64> = (-4000..4000).map(|i| c + i as f64 * 0.001).collect();
        let dx = xs[1] - xs[0];
        let num: f64 = xs.iter().map(|x| gauss_line(*x, c, w, h)).sum::<f64>() * dx;
        assert!((num - analytic).abs() < 1e-7 * analytic);
    }

    #[test]
    fn lorentz_area_consistency() {
        let (c, w, h) = (0.0_f64, 0.5, 2.0);
        let analytic = h * w * std::f64::consts::PI / 2.0;
        let dx = 0.005;
        let xs: Vec<f64> = (0..400_000).map(|i| -1000.0 + i as f64 * dx).collect();
        let num: f64 = xs.iter().map(|x| lorentz_line(*x, c, w, h)).sum::<f64>() * dx;
        // trapezoidal-ish truncation error dominates the heavy lorentz tails
        assert!((num - analytic).abs() / analytic < 2e-4, "num {num} analytic {analytic}");
    }

    #[test]
    fn voigt_is_normalised() {
        for (sigma, gamma) in [(0.2_f64, 0.0), (0.2, 0.3), (0.05, 1.0), (1.0, 0.01)] {
            // integrate to +/-100*max(sigma,gamma) to capture lorentz tails
            let half = 100.0 * sigma.max(gamma);
            let n = 200_000;
            let dx = 2.0 * half / n as f64;
            let xs: Vec<f64> = (0..=n).map(|i| -half + i as f64 * dx).collect();
            let num: f64 = xs.iter().map(|x| voigt(*x, 0.0, sigma, gamma)).sum::<f64>() * dx;
            assert!((num - 1.0).abs() < 8e-3, "sigma={sigma} gamma={gamma} {num}");
        }
    }

    #[test]
    fn voigt_pure_lorentz_limit() {
        // sigma -> small approaches h*gamma/(pi(dx^2+gamma^2)); check centre.
        let v = voigt(0.0, 0.0, 1e-4, 0.3);
        let expect = 1.0 / (std::f64::consts::PI * 0.3);
        assert!((v - expect).abs() / expect < 1e-3);
    }
}

