//! Deterministic pseudo-random generator (xorshift128+) with Box-Muller
//! normals.  Fixed seed by construction; used by the synthetic curve
//! generator and tests.

pub const SYNTH_SEED: u64 = 20260921;
pub const RNG_VERSION: &str = "rng-xorshift128plus-v1";

pub struct Rng {
    s: [u64; 2],
    spare: Option<f64>,
}

impl Rng {
    pub fn new(seed: u64) -> Self {
        let mut x = seed | 1;
        // splitmix64 to populate both words
        let mut next = || {
            x = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut z = x;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            z ^ (z >> 31)
        };
        Rng {
            s: [next(), next() | 1],
            spare: None,
        }
    }

    #[inline]
    pub fn u64(&mut self) -> u64 {
        let mut s1 = self.s[0];
        let s0 = self.s[1];
        self.s[0] = s0;
        s1 ^= s1 << 23;
        self.s[1] = s1 ^ s0 ^ (s1 >> 18) ^ (s0 >> 5);
        self.s[1].wrapping_add(s0)
    }

    /// Uniform in [0, 1).
    pub fn uniform(&mut self) -> f64 {
        (self.u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Standard normal.
    pub fn normal(&mut self) -> f64 {
        if let Some(z) = self.spare.take() {
            return z;
        }
        loop {
            let u = self.uniform() * 2.0 - 1.0;
            let v = self.uniform() * 2.0 - 1.0;
            let s = u * u + v * v;
            if s < 1.0 && s > 0.0 {
                let m = (-2.0 * s.ln() / s).sqrt();
                self.spare = Some(v * m);
                return u * m;
            }
        }
    }
}

/// Fixed-seed synthetic spectrum (CSV text + description of truth).
///
/// The curve spans 380-720 nm (a wavelength axis even though the label is
/// generic), with two overlapping Gaussians, a Lorentzian, a sloping
/// quadratic baseline, mild Gaussian noise and a handful of injected
/// cosmic-ray spikes.  Truth parameters are embedded in the metadata JSON.
pub fn synthetic_csv() -> (String, serde_json::Value) {
    const N: usize = 900;
    let mut rng = Rng::new(SYNTH_SEED);
    let x0 = 380.0_f64;
    let dx = (720.0 - x0) / (N as f64 - 1.0);
    let peaks = [
        ("gauss", 500.0_f64, 8.0_f64, 1.2_f64),
        ("gauss", 512.0, 10.0, 0.8),
        ("lorentz", 610.0, 14.0, 0.55),
    ];
    let cosmic = [123usize, 455, 701];
    let mut rows = Vec::with_capacity(N);
    for i in 0..N {
        let x = x0 + i as f64 * dx;
        let mut y = 0.15 - 0.0004 * (x - 550.0) + 0.000_002 * (x - 550.0).powi(2);
        for (kind, c, w, h) in peaks {
            y += match kind {
                "gauss" => crate::model::gauss_line(x, c, w, h),
                _ => crate::model::lorentz_line(x, c, w, h),
            };
        }
        y += 0.01 * rng.normal();
        if cosmic.contains(&i) {
            y += 0.6 + 0.1 * rng.uniform();
        }
        rows.push(format!("{:.4},{:.6}", x, y));
    }
    let csv = "wavelength_nm,intensity\n".to_string() + &rows.join("\n") + "\n";
    let meta = serde_json::json!({
        "generator": RNG_VERSION,
        "seed": SYNTH_SEED,
        "n": N,
        "x_range_nm": [x0, 720.0],
        "noise_sigma": 0.01,
        "cosmic_ray_rows": cosmic,
        "baseline": {"form": "quadratic", "coef_about_550": [0.15, -0.0004, 0.000002]},
        "peaks": peaks.map(|p| serde_json::json!({"kind": p.0, "center_nm": p.1,
            "fwhm_nm": p.2, "height": p.3})),
    });
    (csv, meta)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic() {
        let mut a = Rng::new(SYNTH_SEED);
        let stream: Vec<u64> = (0..8).map(|_| a.u64()).collect();
        for (i, v) in stream.iter().enumerate() {
            let mut r = Rng::new(SYNTH_SEED);
            for _ in 0..i {
                r.u64();
            }
            assert_eq!(r.u64(), *v);
        }
    }

    #[test]
    fn synth_is_deterministic() {
        let (a, _) = synthetic_csv();
        let (b, _) = synthetic_csv();
        assert_eq!(a, b);
        assert_eq!(a.lines().count(), 901);
    }
}



#[cfg(test)]
mod stats_tests {
    use super::*;
    #[test]
    fn normal_mean_variance() {
        let mut r = Rng::new(7001);
        let n = 2_000_000u64;
        let (m, v) = (0..n).fold((0.0f64, 0.0f64), |(s, s2), _| {
            let z = r.normal();
            (s + z, s2 + z * z)
        });
        assert!((m / n as f64).abs() < 1e-3);
        assert!((v / n as f64 - 1.0).abs() < 1e-2);
    }
}
