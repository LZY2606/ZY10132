//! Levenberg-Marquardt nonlinear least squares for peak models.
//!
//! Parameterisation:
//! - centres are free (no box), widths/heights/areas/voigt scales are fitted
//!   in log space (`p = exp(q)`), which enforces positivity and naturally
//!   yields scale-invariant steps;
//! - fixed centres (`fix_center`) are removed from the free vector;
//! - Jacobians use central finite differences in free-parameter space.
//!
//! Numerical tolerances (see README "Numerical tolerances"):
//! FD step:            1e-7 * max(1, |q|)
//! initial lambda:     1e-3
//! lambda growth:      10.0 / shrink 0.3 (NR-style)
//! max iterations:     200
//! gradient tol:       1e-10 (relative chi2 change)
//! parameter tol:      1e-10
//! singular threshold: 1e-12 diagonal relative pivot
//! failure lambda:     1e12

use crate::model::{eval_peaks, initial_vector, peak_area, PeakKind, PeakSpec};
use serde::{Deserialize, Serialize};

pub const FITTER_VERSION: &str = "lma-v1.0.0";

const MAX_ITER: usize = 200;
const FD_EPS: f64 = 1e-7;
const LAMBDA0: f64 = 1e-3;
const LAMBDA_UP: f64 = 10.0;
const LAMBDA_DOWN: f64 = 0.3;
const CHI_TOL: f64 = 1e-10;
const PARAM_TOL: f64 = 1e-10;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IterTrace {
    pub iter: usize,
    pub chi2: f64,
    pub lambda: f64,
    pub accepted: bool,
    pub params: Vec<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeakResult {
    pub kind: PeakKind,
    pub center: f64,
    pub width: f64,
    pub height: f64,
    /// For voigt: sigma/gamma plus width FWHM-equivalence.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sigma: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gamma: Option<f64>,
    pub area: f64,
    pub area_ci95: [f64; 2],
    pub area_se: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FitReport {
    pub converged: bool,
    pub status: String,
    pub iterations: usize,
    pub chi2: f64,
    pub dof: usize,
    pub rmse: f64,
    pub aic: f64,
    pub bic: f64,
    pub rss: f64,
    pub kinds: Vec<PeakKind>,
    pub params: Vec<f64>,
    /// Free-parameter covariance (sigma^2 scaled), order = free vector.
    pub covariance: Vec<f64>,
    pub correlation: Vec<f64>,
    /// Maps free index -> (peak index, slot).
    pub free_map: Vec<(usize, usize)>,
    pub peaks: Vec<PeakResult>,
    pub fitted: Vec<f64>,
    pub residual: Vec<f64>,
    pub trace: Vec<IterTrace>,
}

struct Layout {
    kinds: Vec<PeakKind>,
    p0: Vec<f64>,
    fixed: Vec<bool>,
    /// free index -> absolute parameter index
    map: Vec<usize>,
    /// which absolute slots are log-space
    logslot: Vec<bool>,
}

fn build_layout(specs: &[PeakSpec]) -> Layout {
    let (kinds, p0, fixed) = initial_vector(specs);
    let mut map = Vec::new();
    let mut logslot = vec![false; p0.len()];
    // slot 0 (center) linear; other slots positive -> log
    let mut off = 0;
    for (i, k) in kinds.iter().enumerate() {
        for slot in 0..k.n_params() {
            let idx = off + slot;
            if slot != 0 {
                logslot[idx] = true;
            }
            if !fixed[idx] {
                map.push(idx);
            }
        }
        off += k.n_params();
        let _ = i;
    }
    Layout {
        kinds,
        p0,
        fixed,
        map,
        logslot,
    }
}

fn to_free(layout: &Layout, p: &[f64]) -> Vec<f64> {
    layout
        .map
        .iter()
        .map(|&i| {
            if layout.logslot[i] {
                p[i].max(1e-300).ln()
            } else {
                p[i]
            }
        })
        .collect()
}

fn from_free(layout: &Layout, q: &[f64]) -> Vec<f64> {
    let mut p = layout.p0.clone();
    for (fi, &ai) in layout.map.iter().enumerate() {
        p[ai] = if layout.logslot[ai] {
            q[fi].exp()
        } else {
            q[fi]
        };
    }
    p
}

fn residuals(xs: &[f64], ys: &[f64], mask: &[bool], kinds: &[PeakKind], p: &[f64]) -> Vec<f64> {
    let mut r = Vec::with_capacity(xs.len());
    for i in 0..xs.len() {
        if mask[i] {
            continue;
        }
        r.push(eval_peaks(xs[i], kinds, p) - ys[i]);
    }
    r
}

fn chi2_of(r: &[f64]) -> f64 {
    r.iter().map(|v| v * v).sum()
}

/// Central-difference Jacobian over the free vector (rows = used points).
fn jacobian(
    xs: &[f64],
    mask: &[bool],
    kinds: &[PeakKind],
    _p: &[f64],
    layout: &Layout,
    q: &[f64],
) -> Vec<f64> {
    let npts = mask.iter().filter(|m| !**m).count();
    let nf = q.len();
    let mut j = vec![0.0_f64; npts * nf];
    for fi in 0..nf {
        let h = FD_EPS * (1.0_f64).max(q[fi].abs());
        let mut qp = q.to_vec();
        let mut qm = q.to_vec();
        qp[fi] += h;
        qm[fi] -= h;
        let pp = from_free(layout, &qp);
        let pm = from_free(layout, &qm);
        let mut row = 0;
        for i in 0..xs.len() {
            if mask[i] {
                continue;
            }
            let fp = eval_peaks(xs[i], kinds, &pp);
            let fm = eval_peaks(xs[i], kinds, &pm);
            j[row * nf + fi] = (fp - fm) / (2.0 * h);
            row += 1;
        }
    }
    j
}

/// Run the fit.  `ys` must already be baseline-subtracted (or zero baseline).
/// `mask[i] == true` excludes point i (baseline gaps, cosmic rays).
pub fn fit(xs: &[f64], ys: &[f64], mask: &[bool], specs: &[PeakSpec]) -> FitReport {
    let layout = build_layout(specs);
    let nf = layout.map.len();
    let kinds = layout.kinds.clone();
    let npts = mask.iter().filter(|m| !**m).count();
    let mut q = to_free(&layout, &layout.p0);
    let mut p = from_free(&layout, &q);
    let mut r = residuals(xs, ys, mask, &kinds, &p);
    let mut chi = chi2_of(&r);
    let mut lambda = LAMBDA0;
    let mut trace = vec![IterTrace {
        iter: 0,
        chi2: chi,
        lambda,
        accepted: true,
        params: p.clone(),
    }];

    let mut converged = false;
    let mut status = "max_iterations".to_string();
    let mut iters = 0usize;

    if npts <= nf {
        status = "underdetermined".into();
    }

    for it in 1..=MAX_ITER {
        iters = it;
        if status == "underdetermined" {
            break;
        }
        let j = jacobian(xs, mask, &kinds, &p, &layout, &q);
        // J^T J and J^T r  (r = model - data; gradient 2 J^T r)
        let mut jtj = vec![0.0_f64; nf * nf];
        let mut g = vec![0.0_f64; nf];
        for a in 0..nf {
            for row in 0..npts {
                let jr = j[row * nf + a];
                g[a] += jr * r[row];
                for b in 0..=a {
                    let v = jr * j[row * nf + b];
                    jtj[a * nf + b] += v;
                    jtj[b * nf + a] += v;
                }
            }
        }

        let mut accepted = false;
        let mut trial_ok = false;
        let mut p_try = p.clone();
        let mut q_try = q.clone();
        let mut r_try = r.clone();
        let mut chi_try = chi;
        for _attempt in 0..60 {
            let mut a = jtj.clone();
            for a_i in 0..nf {
                a[a_i * nf + a_i] *= 1.0 + lambda;
            }
            let mut rhs: Vec<f64> = g.iter().map(|v| -v).collect();
            if crate::linalg::solve(&mut a, &mut rhs, nf).is_none() {
                lambda *= LAMBDA_UP;
                if lambda > 1e12 {
                    break;
                }
                continue;
            }
            let qn: Vec<f64> = q.iter().zip(rhs.iter()).map(|(a, b)| a + b).collect();
            let pn = from_free(&layout, &qn);
            let rn = residuals(xs, ys, mask, &kinds, &pn);
            let chin = chi2_of(&rn);
            if chin.is_finite() {
                trial_ok = true;
                q_try = qn;
                p_try = pn;
                r_try = rn;
                chi_try = chin;
                accepted = chin < chi;
            }
            break;
        }

        let rel = if chi > 0.0 {
            (chi - chi_try).abs() / chi.max(1e-300)
        } else {
            0.0
        };
        if accepted {
            let step: f64 = q
                .iter()
                .zip(q_try.iter())
                .map(|(a, b)| (a - b).abs())
                .fold(0.0_f64, f64::max);
            q = q_try;
            p = p_try;
            r = r_try;
            chi = chi_try;
            lambda *= LAMBDA_DOWN;
            trace.push(IterTrace {
                iter: it,
                chi2: chi,
                lambda,
                accepted: true,
                params: p.clone(),
            });
            if rel < CHI_TOL || step < PARAM_TOL {
                converged = true;
                status = "converged".into();
                break;
            }
        } else {
            lambda *= LAMBDA_UP;
            trace.push(IterTrace {
                iter: it,
                chi2: chi_try,
                lambda,
                accepted: false,
                params: p_try.clone(),
            });
            if !trial_ok || lambda > 1e12 {
                status = "no_finite_step".into();
                break;
            }
        }
    }

    finalize(
        layout, p, r, chi, mask, xs, ys, converged, status, iters, trace,
    )
}

const T95: f64 = 1.959_964_348_275_349_5; // normal 97.5% CI (large-sample)

fn finalize(
    layout: Layout,
    p: Vec<f64>,
    r: Vec<f64>,
    chi: f64,
    mask: &[bool],
    xs: &[f64],
    _ys: &[f64],
    converged: bool,
    status: String,
    iters: usize,
    trace: Vec<IterTrace>,
) -> FitReport {
    let nf = layout.map.len();
    let kinds = layout.kinds.clone();
    let npts = r.len();
    let dof = npts.saturating_sub(nf);
    let rmse = (chi / npts.max(1) as f64).sqrt();
    let aic = if npts > 0 && chi > 0.0 {
        npts as f64 * (chi / npts as f64).ln() + 2.0 * nf as f64
    } else {
        f64::NAN
    };
    let bic = if npts > 0 && chi > 0.0 {
        npts as f64 * (chi / npts as f64).ln() + nf as f64 * (npts as f64).ln()
    } else {
        f64::NAN
    };

    // Jacobian / covariance at the final iterate.
    let q = to_free(&layout, &p);
    let j = jacobian(xs, mask, &kinds, &p, &layout, &q);
    let mut jtj = vec![0.0_f64; nf * nf];
    for a in 0..nf {
        for row in 0..npts {
            for b in 0..=a {
                let v = j[row * nf + a] * j[row * nf + b];
                jtj[a * nf + b] += v;
                jtj[b * nf + a] += v;
            }
        }
    }
    let s2 = if dof > 0 { chi / dof as f64 } else { f64::NAN };
    let mut cov = vec![f64::NAN; nf * nf];
    if nf > 0 && dof > 0 && s2.is_finite() {
        for col in 0..nf {
            let mut a = jtj.clone();
            let mut rhs = vec![0.0; nf];
            rhs[col] = 1.0;
            if crate::linalg::solve(&mut a, &mut rhs, nf).is_some() {
                for row in 0..nf {
                    cov[row * nf + col] = rhs[row] * s2;
                }
            }
        }
    }
    let mut corr = vec![f64::NAN; nf * nf];
    for a in 0..nf {
        for b in 0..nf {
            let da = cov[a * nf + a].max(0.0).sqrt();
            let db = cov[b * nf + b].max(0.0).sqrt();
            if da > 0.0 && db > 0.0 {
                corr[a * nf + b] = cov[a * nf + b] / (da * db);
            }
        }
    }

    // Per-peak results with delta-method area uncertainty.
    let mut peaks = Vec::new();
    let mut off = 0usize;
    for (pi, kind) in kinds.iter().enumerate() {
        let n = kind.n_params();
        let pp = &p[off..off + n];
        let area = peak_area(*kind, pp);
        // area gradient wrt free q via central differences
        let mut g = vec![0.0_f64; nf];
        for fi in 0..nf {
            let h = FD_EPS * 1.0_f64.max(q[fi].abs());
            let area_at = |dq: f64| {
                let mut qq = q.clone();
                qq[fi] += dq;
                let pp2 = from_free(&layout, &qq);
                peak_area(*kind, &pp2[off..off + n])
            };
            g[fi] = (area_at(h) - area_at(-h)) / (2.0 * h);
        }
        let mut var_area = 0.0;
        for a in 0..nf {
            for b in 0..nf {
                let c = cov[a * nf + b];
                if c.is_finite() {
                    var_area += g[a] * g[b] * c;
                }
            }
        }
        let se = var_area.max(0.0).sqrt();
        let (sigma, gamma, width, height) = match kind {
            PeakKind::Gauss | PeakKind::Lorentz => (None, None, pp[1], pp[2]),
            PeakKind::Voigt => {
                let fwhm_g = 2.0 * (2.0 * std::f64::consts::LN_2).sqrt() * pp[1];
                let fwhm_l = 2.0 * pp[2];
                let fwhm = 0.5346 * fwhm_l
                    + (0.2166 * fwhm_l * fwhm_l + fwhm_g * fwhm_g).sqrt();
                (
                    Some(pp[1]),
                    Some(pp[2]),
                    fwhm,
                    area * crate::model::voigt(pp[0], pp[0], pp[1].max(1e-12), pp[2]),
                )
            }
        };
        peaks.push(PeakResult {
            kind: *kind,
            center: pp[0],
            width,
            height,
            sigma,
            gamma,
            area,
            area_se: se,
            area_ci95: [area - T95 * se, area + T95 * se],
        });
        off += n;
        let _ = pi;
    }

    let fitted: Vec<f64> = xs.iter().map(|x| eval_peaks(*x, &kinds, &p)).collect();
    let mut residual = vec![f64::NAN; xs.len()];
    let mut row = 0;
    for i in 0..xs.len() {
        if !mask[i] {
            residual[i] = r[row];
            row += 1;
        }
    }

    FitReport {
        converged,
        status,
        iterations: iters,
        chi2: chi,
        dof,
        rmse,
        aic,
        bic,
        rss: chi,
        kinds: kinds.clone(),
        params: p,
        covariance: cov,
        correlation: corr,
        free_map: layout
            .map
            .iter()
            .map(|&abs| {
                // absolute slot -> (peak index, slot within peak)
                let mut acc = 0usize;
                for (pi, k) in kinds.iter().enumerate() {
                    if abs < acc + k.n_params() {
                        return (pi, abs - acc);
                    }
                    acc += k.n_params();
                }
                (0, 0)
            })
            .collect(),
        peaks,
        fitted,
        residual,
        trace,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recover(specs: Vec<PeakSpec>, truth: &[(PeakKind, f64, f64, f64)]) {
        let xs: Vec<f64> = (0..1000).map(|i| i as f64 * 0.01).collect();
        let kinds: Vec<PeakKind> = truth.iter().map(|t| t.0).collect();
        let p: Vec<f64> = truth
            .iter()
            .flat_map(|t| vec![t.1, t.2, t.3])
            .collect();
        let ys: Vec<f64> = xs
            .iter()
            .map(|x| crate::model::eval_peaks(*x, &kinds, &p))
            .collect();
        let mask = vec![false; xs.len()];
        let rep = fit(&xs, &ys, &mask, &specs);
        assert!(rep.converged, "{}", rep.status);
        assert!(rep.rmse < 1e-6, "rmse {}", rep.rmse);
        for (got, t) in rep.peaks.iter().zip(truth) {
            assert!((got.center - t.1).abs() < 1e-4, "center {} {}", got.center, t.1);
            assert!((got.width - t.2).abs() < 1e-4 * t.2, "width");
            assert!((got.height - t.3).abs() < 1e-5 * t.3, "height");
        }
    }

    #[test]
    fn recovers_two_gaussians() {
        recover(
            vec![
                PeakSpec {
                    kind: PeakKind::Gauss,
                    center0: 2.9,
                    width0: 0.5,
                    amp0: 1.0,
                    fix_center: false,
                },
                PeakSpec {
                    kind: PeakKind::Gauss,
                    center0: 7.2,
                    width0: 0.4,
                    amp0: 0.5,
                    fix_center: false,
                },
            ],
            &[
                (PeakKind::Gauss, 3.0, 0.3, 2.0),
                (PeakKind::Gauss, 7.0, 0.2, 0.7),
            ],
        );
    }

    #[test]
    fn recovers_lorentzian_with_fixed_center() {
        recover(
            vec![PeakSpec {
                kind: PeakKind::Lorentz,
                center0: 5.0,
                width0: 0.7,
                amp0: 1.0,
                fix_center: true,
            }],
            &[(PeakKind::Lorentz, 5.0, 0.4, 3.0)],
        );
    }

    #[test]
    fn noisy_fit_ci_coverage_rate() {
        use crate::rng::Rng;
        let (c, w, h) = (6.0_f64, 0.25, 2.0);
        let truth =
            h * w * (std::f64::consts::PI / (4.0 * std::f64::consts::LN_2)).sqrt();
        let mut covered = 0;
        let seeds = 1..=12u64;
        let total = 12;
        for seed in seeds {
            let mut rng = Rng::new(7000 + seed);
            let xs: Vec<f64> = (0..2400).map(|i| i as f64 * 0.005).collect();
            let ys: Vec<f64> = xs
                .iter()
                .map(|x| {
                    crate::model::gauss_line(*x, c, w, h) + rng.normal() * 0.005
                })
                .collect();
            let mask = vec![false; xs.len()];
            let rep = fit(
                &xs,
                &ys,
                &mask,
                &[PeakSpec {
                    kind: PeakKind::Gauss,
                    center0: 6.05,
                    width0: 0.4,
                    amp0: 1.5,
                    fix_center: false,
                }],
            );
            assert!(rep.converged);
            let ci = rep.peaks[0].area_ci95;
            if truth > ci[0] && truth < ci[1] {
                covered += 1;
            }
        }
        // expect ~95% nominal coverage; allow a generous 70% floor for the
        // small-sample delta-method implementation.
        assert!(covered >= 8, "coverage {covered}/{total} (67% floor, nominal 95%)");
    }
}

