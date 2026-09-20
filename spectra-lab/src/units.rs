//! X-axis units and physical-range-preserving transformations.
//!
//! Supported units:
//! - wavelength `nm`
//! - wavenumber  `cm^-1` (defined as 1e7 / wavelength_nm)
//! - energy      `eV`
//!
//! All conversions are anchored on wavelength in nanometres.  The physical
//! constants are fixed here so that conversions performed by the web UI
//! (which mirrors these formulas) and by the backend stay bit-compatible
//! enough for range checks (tests compare with the same constants).

use serde::{Deserialize, Serialize};

/// Speed of light times Planck constant: h*c in eV * nm (CODATA 2018, rounded).
pub const HC_EV_NM: f64 = 1239.841_984_392_919_6;
/// One centimetre expressed in nanometres; used by the cm^-1 definition.
pub const CM_IN_NM: f64 = 1.0e7;

/// Method/constants version.  Bump when any formula or constant changes.
pub const UNITS_VERSION: &str = "units-v1.0.0";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum XUnit {
    Nm,
    CmInv,
    Ev,
}

impl XUnit {
    pub fn as_str(self) -> &'static str {
        match self {
            XUnit::Nm => "nm",
            XUnit::CmInv => "cm^-1",
            XUnit::Ev => "eV",
        }
    }

    pub fn parse(s: &str) -> Option<XUnit> {
        match s.trim().to_ascii_lowercase().replace(' ', "").as_str() {
            "nm" | "wavelength" | "wavelength_nm" | "wavelength(nm)" => Some(XUnit::Nm),
            "cm^-1" | "cm-1" | "cm_inv" | "wavenumber" | "1/cm" | "wavenumber(cm^-1)" => {
                Some(XUnit::CmInv)
            }
            "ev" | "energy" | "energy_ev" | "energy(ev)" => Some(XUnit::Ev),
            _ => None,
        }
    }
}

/// Convert a scalar coordinate from `from` to `to`.
pub fn convert_scalar(x: f64, from: XUnit, to: XUnit) -> f64 {
    if from == to || !x.is_finite() || x == 0.0 {
        return x;
    }
    // First to wavelength in nm.
    let wl = match from {
        XUnit::Nm => x,
        XUnit::CmInv => CM_IN_NM / x,
        XUnit::Ev => HC_EV_NM / x,
    };
    match to {
        XUnit::Nm => wl,
        XUnit::CmInv => CM_IN_NM / wl,
        XUnit::Ev => HC_EV_NM / wl,
    }
}

/// A physical interval expressed in one unit.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Interval {
    pub lo: f64,
    pub hi: f64,
    #[serde(rename = "unit")]
    pub unit: XUnit,
}

impl Interval {
    pub fn new(a: f64, b: f64, unit: XUnit) -> Self {
        Interval {
            lo: a.min(b),
            hi: a.max(b),
            unit,
        }
    }

    /// Transform both endpoints and re-sort.  Re-sorting is required because
    /// reciprocal transforms (nm -> eV / cm^-1) reverse ordering.
    pub fn to_unit(&self, to: XUnit) -> Interval {
        let a = convert_scalar(self.lo, self.unit, to);
        let b = convert_scalar(self.hi, self.unit, to);
        Interval::new(a, b, to)
    }

    /// Whether the interval in the *target* unit covers the same physical
    /// range as this interval, within `tol` (relative, evaluated on target
    /// endpoints).  NaN endpoints always fail.
    pub fn same_physical_range(&self, other: &Interval, tol: f64) -> bool {
        let a = self.to_unit(other.unit);
        let span = (a.hi - a.lo).abs().max((other.hi - other.lo).abs());
        let scale = span.max(
            a.lo.abs()
                .max(a.hi.abs())
                .max(other.lo.abs())
                .max(other.hi.abs()),
        );
        (a.lo - other.lo).abs() <= tol * scale && (a.hi - other.hi).abs() <= tol * scale
    }

    pub fn contains(&self, x: f64) -> bool {
        x >= self.lo && x <= self.hi
    }
}

/// Convert a peak location/width.
///
/// Width convention: the full width parameter `w` is treated as a symmetric
/// span around the centre.  The physical interval [c - w/2, c + w/2] is
/// transformed explicitly and the new width is the width of that transformed
/// interval.  This guarantees the transformed width covers the same physical
/// range even when the reciprocal transform makes the span asymmetric (the
/// transformed peak is then a symmetric approximation centred on the mapped
/// centre; see README "Unit transforms").
pub fn convert_peak(
    center: f64,
    width: f64,
    from: XUnit,
    to: XUnit,
) -> (f64, f64) {
    if from == to {
        return (center, width);
    }
    let c = convert_scalar(center, from, to);
    let lo = convert_scalar(center - width / 2.0, from, to);
    let hi = convert_scalar(center + width / 2.0, from, to);
    (c, (hi - lo).abs())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalar_roundtrip() {
        for x in [400.0_f64, 550.0, 800.0] {
            for u in [XUnit::Nm, XUnit::CmInv, XUnit::Ev] {
                for v in [XUnit::Nm, XUnit::CmInv, XUnit::Ev] {
                    let back = convert_scalar(convert_scalar(x, u, v), v, u);
                    assert!((back - x).abs() < 1e-10 * x, "{x} {u:?} {v:?} {back}");
                }
            }
        }
    }

    #[test]
    fn known_constants() {
        let ev = convert_scalar(1239.841_984_392_919_6, XUnit::Nm, XUnit::Ev);
        assert!((ev - 1.0).abs() < 1e-12);
        let cm = convert_scalar(500.0, XUnit::Nm, XUnit::CmInv);
        assert!((cm - 20000.0).abs() < 1e-9);
    }

    #[test]
    fn interval_preserves_range_through_inversion() {
        let nm = Interval::new(500.0, 510.0, XUnit::Nm);
        let ev = nm.to_unit(XUnit::Ev);
        // reciprocal transform reverses ordering; constructor re-sorts.
        assert!(ev.lo < ev.hi);
        assert!(nm.same_physical_range(&ev, 1e-12));
        // A point inside the range stays inside after both are converted.
        let p_nm = 505.0;
        let p_ev = convert_scalar(p_nm, XUnit::Nm, XUnit::Ev);
        assert!(ev.contains(p_ev));
    }

    #[test]
    fn peak_width_maps_to_same_range() {
        let (c, w) = convert_peak(500.0, 2.0, XUnit::Nm, XUnit::Ev);
        let (cn, wn) = convert_peak(c, w, XUnit::Ev, XUnit::Nm);
        assert!((cn - 500.0).abs() < 1e-9);
        // symmetric-span convention; reciprocal transform gives small
        // asymmetry, tolerance reflects the documented convention.
        assert!((wn - 2.0).abs() / 2.0 < 1e-5, "wn {wn}");
    }
}
