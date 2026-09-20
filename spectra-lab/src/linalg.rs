//! Small linear-algebra helpers (no BLAS dependency).

/// Weighted polynomial least squares fit.
///
/// Returns coefficients `a[0..=degree]` with `p(x) = sum a[k] x^k`,
/// evaluated against `xs` already shifted/scaled by `(x - mu)/scale`.
/// Callers map coordinates to a well-conditioned window first.
/// Returns `None` when the normal equations are singular (rank < degree+1).
pub fn polyfit_norm(
    t: &[f64],
    y: &[f64],
    w: &[f64],
    degree: usize,
) -> Option<Vec<f64>> {
    let n = degree + 1;
    let mut a = vec![0.0_f64; n * n];
    let mut b = vec![0.0_f64; n];
    for i in 0..degree + 1 {
        for j in 0..=i {
            let mut s = 0.0;
            for k in 0..t.len() {
                s += w[k] * t[k].powi(i as i32) * t[k].powi(j as i32);
            }
            a[i * n + j] = s;
            a[j * n + i] = s;
        }
        let mut s = 0.0;
        for k in 0..t.len() {
            s += w[k] * t[k].powi(i as i32) * y[k];
        }
        b[i] = s;
    }
    solve_sym_pos(&a, &b, n)
}

/// Solve symmetric positive definite system by Cholesky.
pub fn solve_sym_pos(a: &[f64], b: &[f64], n: usize) -> Option<Vec<f64>> {
    let mut l = vec![0.0_f64; n * n];
    for i in 0..n {
        for j in 0..=i {
            let mut s = a[i * n + j];
            for k in 0..j {
                s -= l[i * n + k] * l[j * n + k];
            }
            if i == j {
                if s <= 0.0 {
                    return None;
                }
                l[i * n + i] = s.sqrt();
            } else {
                l[i * n + j] = s / l[j * n + j];
            }
        }
    }
    // forward
    let mut y = vec![0.0; n];
    for i in 0..n {
        let mut s = b[i];
        for k in 0..i {
            s -= l[i * n + k] * y[k];
        }
        y[i] = s / l[i * n + i];
    }
    // backward
    let mut x = vec![0.0; n];
    for i in (0..n).rev() {
        let mut s = y[i];
        for k in i + 1..n {
            s -= l[k * n + i] * x[k];
        }
        x[i] = s / l[i * n + i];
    }
    Some(x)
}

/// General linear solve with partial pivoting (used for LM step).
/// Returns `None` if exactly singular.
pub fn solve(a: &mut [f64], b: &mut [f64], n: usize) -> Option<()> {
    for col in 0..n {
        let mut piv = col;
        for r in col + 1..n {
            if a[r * n + col].abs() > a[piv * n + col].abs() {
                piv = r;
            }
        }
        if a[piv * n + col] == 0.0 || !a[piv * n + col].is_finite() {
            return None;
        }
        if piv != col {
            for k in 0..n {
                a.swap(col * n + k, piv * n + k);
            }
            b.swap(col, piv);
        }
        for r in col + 1..n {
            let f = a[r * n + col] / a[col * n + col];
            a[r * n + col] = 0.0;
            for k in col + 1..n {
                a[r * n + k] -= f * a[col * n + k];
            }
            b[r] -= f * b[col];
        }
    }
    for i in (0..n).rev() {
        let mut s = b[i];
        for k in i + 1..n {
            s -= a[i * n + k] * b[k];
        }
        b[i] = s / a[i * n + i];
    }
    Some(())
}

/// Gauss-Hermite nodes and weights (physicist's Hermite, weight e^{-x^2}),
/// via the Golub-Welsch Jacobi matrix and an implicit-shift QL sweep with
/// accumulated eigenvectors.
pub fn gauss_hermite(n: usize) -> Vec<(f64, f64)> {
    // 0-based: d[0..n] diagonal (all zero);
    // e[i] = off-diagonal between rows i-1 and i, for i in 1..n.
    let mut d = vec![0.0_f64; n];
    let mut e = vec![0.0_f64; n + 1];
    for i in 1..n {
        e[i] = (i as f64).sqrt() / std::f64::consts::SQRT_2;
    }
    let mut z = vec![0.0_f64; n * n];
    for i in 0..n {
        z[i * n + i] = 1.0;
    }
    tqli(n, &mut d, &mut e, &mut z);
    // d[i] is the eigenvalue whose converged eigenvector is column i of z.
    let mut out: Vec<(f64, f64)> = d
        .iter()
        .enumerate()
        .map(|(i, &node)| (node, std::f64::consts::PI.sqrt() * z[0 * n + i].powi(2)))
        .collect();
    out.sort_by(|p, q| p.0.partial_cmp(&q.0).unwrap());
    out
}

/// Implicit-shift QL for a symmetric tridiagonal matrix, 0-based.
/// `e[i]` is the off-diagonal between rows i-1 and i (`e[0]` unused).
/// Rotations are accumulated in row-major `z`.
fn tqli(n: usize, d: &mut [f64], e: &mut [f64], z: &mut [f64]) {
    for l in 0..n {
        let mut it = 0u32;
        loop {
            // find m: smallest m >= l with e[m+1] negligible.
            let mut m = l;
            while m + 1 < n {
                let scale = d[m].abs() + d[m + 1].abs();
                if e[m + 1].abs() <= 1e-15 * scale {
                    break;
                }
                m += 1;
            }
            if m == l {
                break;
            }
            it += 1;
            assert!(it < 300, "tqli failed to converge");
            let g0 = (d[l + 1] - d[l]) / (2.0 * e[l + 1]);
            let r0 = g0.hypot(1.0);
            let mut g = d[m] - d[l] + e[l + 1] / (g0 + r0.copysign(g0));
            let mut s = 1.0_f64;
            let mut c = 1.0_f64;
            let mut p = 0.0_f64;
            for i in (l + 1..=m).rev() {
                let f = s * e[i];
                let b = c * e[i];
                let r = f.hypot(g);
                e[i + 1] = r;
                s = f / r;
                c = g / r;
                g = d[i] - p;
                let r = (d[i - 1] - g) * s + 2.0 * c * b;
                p = s * r;
                d[i] = g + p;
                g = c * r - b;
                for k in 0..n {
                    let f = z[k * n + i];
                    z[k * n + i] = s * z[k * n + i - 1] + c * f;
                    z[k * n + i - 1] = c * z[k * n + i - 1] - s * f;
                }
            }
            d[l] -= p;
            e[l + 1] = g;
            e[m + 1] = 0.0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gh_nodes_integrate_gaussian() {
        for n in [8usize, 32, 64] {
            let qw = gauss_hermite(n);
            assert_eq!(qw.len(), n);
            let total: f64 = qw.iter().map(|(_, w)| w).sum();
            assert!((total - std::f64::consts::PI.sqrt()).abs() < 1e-12);
            // integral x^2 e^-x2 = sqrt(pi)/2
            let m2: f64 = qw.iter().map(|(t, w)| t * t * w).sum();
            assert!((m2 - std::f64::consts::PI.sqrt() / 2.0).abs() < 1e-10);
            // symmetry
            for i in 0..n / 2 {
                assert!((qw[i].0 + qw[n - 1 - i].0).abs() < 1e-12);
                assert!((qw[i].1 - qw[n - 1 - i].1).abs() < 1e-12);
            }
        }
    }

    #[test]
    fn polyfit_recovers_cubic() {
        let xs: Vec<f64> = (0..40).map(|i| i as f64 * 0.2).collect();
        let ys: Vec<f64> = xs.iter().map(|x| 1.0 - 2.0 * x + 0.5 * x * x).collect();
        let w = vec![1.0; xs.len()];
        let t: Vec<f64> = xs
            .iter()
            .map(|x| (x - xs[xs.len() / 2]) / xs[xs.len() - 1])
            .collect();
        let cf = polyfit_norm(&t, &ys, &w, 2).unwrap();
        for (a, b) in t.iter().zip(ys.iter()) {
            let v = cf[0] + cf[1] * a + cf[2] * a * a;
            assert!((v - b).abs() < 1e-8);
        }
    }
}

