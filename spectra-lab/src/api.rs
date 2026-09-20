//! HTTP API handlers: datasets, schemes, fits, transforms, synthetic data.

use crate::data::{parse_csv, sha256_hex, LOADER_VERSION};
use crate::db::Db;
use crate::fit::FITTER_VERSION;
use crate::model::MODEL_VERSION;
use crate::rng::synthetic_csv;
use crate::scheme::{
    artifact_hash, canonical_json, fingerprint, run_pipeline, SchemeSpec, SCHEME_VERSION,
};
use crate::units::{convert_peak, convert_scalar, XUnit, UNITS_VERSION};
use serde_json::json;
use std::sync::{Arc, Mutex};

pub struct AppState {
    pub db: Db,
}

pub type Shared = Arc<Mutex<AppState>>;

fn json_ok(v: serde_json::Value) -> (u16, String, String) {
    (200, "application/json".to_string(), v.to_string())
}
fn json_err(code: u16, msg: &str) -> (u16, String, String) {
    (
        code,
        "application/json".to_string(),
        json!({"error": msg}).to_string(),
    )
}

fn load_dataset_record(st: &AppState, id: i64) -> Option<String> {
    st.db.get_dataset(id).ok().flatten()
}

/// Parse an upload or the synthetic sample.
pub fn upload(state: &Shared, _q: &str, body: &str) -> (u16, String, String) {
    #[derive(serde::Deserialize)]
    struct Req {
        #[serde(default)]
        csv: Option<String>,
        #[serde(default)]
        synthetic: Option<bool>,
        #[serde(default)]
        x_unit: Option<String>,
        #[serde(default)]
        y_kind: Option<String>,
    }
    let req: Req = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(e) => return json_err(400, &format!("bad json: {e}")),
    };

    let (csv, source, synth_meta) = if req.synthetic == Some(true) {
        let (csv, meta) = synthetic_csv();
        (csv, "synthetic".to_string(), Some(meta))
    } else {
        match req.csv {
            Some(c) => (c, "upload".to_string(), None),
            None => return json_err(400, "missing csv"),
        }
    };

    let input_hash = sha256_hex(csv.as_bytes());
    let sp = match parse_csv(&csv) {
        Ok(s) => s,
        Err(e) => return json_err(400, &e),
    };
    let x_unit = req
        .x_unit
        .as_deref()
        .and_then(XUnit::parse)
        .or_else(|| XUnit::parse(&sp.x_header))
        .unwrap_or(XUnit::Nm);
    let y_kind = req.y_kind.unwrap_or_else(|| {
        let h = sp.y_header.to_ascii_lowercase();
        if h.contains("absorb") {
            "absorbance".into()
        } else {
            "intensity".into()
        }
    });
    let diag_json = serde_json::to_string(&sp.diagnostics).unwrap();

    let st = state.lock().unwrap();
    let id = match st.db.insert_dataset(
        &input_hash,
        x_unit.as_str(),
        &y_kind,
        &sp.x_header,
        &sp.y_header,
        &csv,
        sp.x.len(),
        LOADER_VERSION,
        &diag_json,
        &source,
    ) {
        Ok(id) => id,
        Err(e) => return json_err(500, &e.to_string()),
    };

    json_ok(json!({
        "dataset_id": id,
        "input_hash": input_hash,
        "x_unit": x_unit.as_str(),
        "y_kind": y_kind,
        "x_header": sp.x_header,
        "y_header": sp.y_header,
        "n_points": sp.x.len(),
        "diagnostics": sp.diagnostics,
        "synthetic_truth": synth_meta,
        "versions": {
            "loader": LOADER_VERSION,
            "units": UNITS_VERSION,
            "model": MODEL_VERSION,
            "fitter": FITTER_VERSION,
            "scheme": SCHEME_VERSION,
        },
        "x": sp.x,
        "y": sp.y,
        "source_rows": sp.source_rows,
    }))
}

pub fn datasets(state: &Shared, _q: &str, _b: &str) -> (u16, String, String) {
    let st = state.lock().unwrap();
    match st.db.list_datasets() {
        Ok(rows) => json_ok(json!({
            "datasets": rows
                .into_iter()
                .map(|(id, h, u, n, t)| json!({"id": id, "input_hash": h,
                    "x_unit": u, "n_points": n, "created_at": t}))
                .collect::<Vec<_>>()
        })),
        Err(e) => json_err(500, &e.to_string()),
    }
}

pub fn convert_endpoint(state: &Shared, _q: &str, body: &str) -> (u16, String, String) {
    #[derive(serde::Deserialize)]
    struct Req {
        from: String,
        to: String,
        #[serde(default)]
        x: Option<Vec<f64>>,
        #[serde(default)]
        intervals: Option<Vec<[f64; 2]>>,
        #[serde(default)]
        peaks: Option<Vec<[f64; 2]>>,
    }
    let req: Req = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(e) => return json_err(400, &e.to_string()),
    };
    let _ = state;
    let (from, to) = match (XUnit::parse(&req.from), XUnit::parse(&req.to)) {
        (Some(a), Some(b)) => (a, b),
        _ => return json_err(400, "unknown unit"),
    };
    let xs = req.x.map(|v| {
        v.into_iter()
            .map(|x| convert_scalar(x, from, to))
            .collect::<Vec<_>>()
    });
    let intervals = req.intervals.map(|v| {
        v.into_iter()
            .map(|p| {
                let mut a = convert_scalar(p[0], from, to);
                let mut b = convert_scalar(p[1], from, to);
                if a > b {
                    std::mem::swap(&mut a, &mut b);
                }
                [a, b]
            })
            .collect::<Vec<_>>()
    });
    let peaks = req.peaks.map(|v| {
        v.into_iter()
            .map(|p| {
                let (c, w) = convert_peak(p[0], p[1], from, to);
                [c, w]
            })
            .collect::<Vec<_>>()
    });
    json_ok(json!({"x": xs, "intervals": intervals, "peaks": peaks,
                   "units_version": UNITS_VERSION}))
}

/// Create a scheme (manual).  Adjusting any initial/constraint creates a new
/// immutable row linked via parent_id; nothing is overwritten.
pub fn create_scheme(state: &Shared, _q: &str, body: &str) -> (u16, String, String) {
    #[derive(serde::Deserialize)]
    struct Req {
        dataset_id: i64,
        #[serde(default)]
        parent_id: Option<i64>,
        spec: SchemeSpec,
    }
    let req: Req = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(e) => return json_err(400, &format!("bad json: {e}")),
    };
    let st = state.lock().unwrap();
    let csv = match load_dataset_record(&st, req.dataset_id) {
        Some(c) => c,
        None => return json_err(404, "unknown dataset"),
    };
    let input_hash = sha256_hex(csv.as_bytes());
    let spec_json = canonical_json(&req.spec);
    let fp = fingerprint(&input_hash, &req.spec);
    match st.db.insert_scheme(
        req.dataset_id,
        req.parent_id,
        &spec_json,
        &fp,
        "manual",
    ) {
        Ok(id) => json_ok(json!({"scheme_id": id, "fingerprint": fp})),
        Err(e) => json_err(500, &e.to_string()),
    }
}

pub fn list_schemes(state: &Shared, q: &str, _b: &str) -> (u16, String, String) {
    let did = match q.split('=').nth(1).and_then(|v| v.parse::<i64>().ok()) {
        Some(v) => v,
        None => return json_err(400, "need dataset_id"),
    };
    let st = state.lock().unwrap();
    match st.db.list_schemes(did) {
        Ok(rows) => json_ok(json!({"schemes": rows.into_iter().map(
            |(id, spec, origin, accepted, parent, fp)| json!({
                "id": id, "spec": serde_json::from_str::<serde_json::Value>(&spec).unwrap(),
                "origin": origin, "accepted": accepted != 0,
                "parent_id": parent, "fingerprint": fp
            })).collect::<Vec<_>>()})),
        Err(e) => json_err(500, &e.to_string()),
    }
}

pub fn accept_scheme(state: &Shared, _q: &str, body: &str) -> (u16, String, String) {
    let v: serde_json::Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(e) => return json_err(400, &e.to_string()),
    };
    let id = match v.get("scheme_id").and_then(|x| x.as_i64()) {
        Some(v) => v,
        None => return json_err(400, "need scheme_id"),
    };
    let accept = v.get("accepted").and_then(|x| x.as_bool()).unwrap_or(true);
    // Auto candidates can never be silently promoted.  The UI must pass an
    // explicit confirmation flag; even then the promotion is an auditable
    // manual action recorded against the same immutable scheme row.
    if accept
        && !v.get("confirm_promote_auto").and_then(|x| x.as_bool()).unwrap_or(false)
    {
        let origin = state
            .lock()
            .unwrap()
            .db
            .scheme_origin(id)
            .ok()
            .flatten()
            .unwrap_or_default();
        if origin == "auto_candidate" {
            return json_err(
                409,
                "auto_candidate requires confirm_promote_auto=true (manual confirmation)",
            );
        }
    }
    let st = state.lock().unwrap();
    match st.db.set_scheme_accepted(id, accept) {
        Ok(()) => json_ok(json!({"scheme_id": id, "accepted": accept})),
        Err(e) => json_err(500, &e.to_string()),
    }
}

/// Run (or replay the cached result of) a fit.  Dedup key is
/// (input, initials, constraints, algorithm versions).  A non-converged
/// fit is stored and returned with converged=false but never marked
/// accepted.
pub fn run_fit(state: &Shared, _q: &str, body: &str) -> (u16, String, String) {
    #[derive(serde::Deserialize)]
    struct Req {
        dataset_id: i64,
        spec: SchemeSpec,
        #[serde(default)]
        auto_candidate: bool,
    }
    let req: Req = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(e) => return json_err(400, &format!("bad json: {e}")),
    };

    let (csv, input_hash) = {
        let st = state.lock().unwrap();
        match load_dataset_record(&st, req.dataset_id) {
            Some(c) => (c.clone(), sha256_hex(c.as_bytes())),
            None => return json_err(404, "unknown dataset"),
        }
    };
    let sp = match parse_csv(&csv) {
        Ok(s) => s,
        Err(e) => return json_err(400, &e),
    };
    if sp.diagnostics.too_few_rows {
        return json_err(400, "too few finite rows for fitting");
    }
    let fp = fingerprint(&input_hash, &req.spec);

    {
        let st = state.lock().unwrap();
        // Fast path: already computed.
        if let Ok(Some((_, report))) = st.db.get_fit_for_job(&fp) {
            return json_ok(json!({"fingerprint": fp, "replayed": true,
                                  "report": serde_json::from_str::<serde_json::Value>(&report).unwrap()}));
        }
    }

    // Create scheme row (auto candidates are tagged and never auto-accepted).
    let scheme_id = {
        let st = state.lock().unwrap();
        let spec_json = canonical_json(&req.spec);
        let origin = if req.auto_candidate { "auto_candidate" } else { "manual" };
        match st
            .db
            .insert_scheme(req.dataset_id, None, &spec_json, &fp, origin)
        {
            Ok(id) => id,
            // UNIQUE not on (dataset,fingerprint) for schemes, so continue
            Err(e) => return json_err(500, &e.to_string()),
        }
    };

    let prepared = match run_pipeline(&sp.x, &sp.y, &req.spec) {
        Ok(p) => p,
        Err(e) => return json_err(400, &e),
    };

    let report_json = serde_json::to_string(&prepared.report).unwrap();
    let baseline_json = canonical_json(&req.spec.baseline);
    let x_json = serde_json::to_string(&sp.x).unwrap();
    let base_hash = artifact_hash(
        &input_hash,
        None,
        MODEL_VERSION,
        &baseline_json,
        &sp.x,
        &prepared.baseline,
    );
    let smooth_art = if let Some(sm) = &prepared.smoothed {
        Some((
            "smoothed".to_string(),
            MODEL_VERSION.to_string(),
            format!("{{\"smooth_sigma\":{}}}", req.spec.smooth_sigma),
            input_hash.clone(),
            Some(base_hash.clone()),
            artifact_hash(
                &input_hash,
                Some(&base_hash),
                MODEL_VERSION,
                &format!("{{\"smooth_sigma\":{}}}", req.spec.smooth_sigma),
                &sp.x,
                sm,
            ),
            x_json.clone(),
            serde_json::to_string(sm).unwrap(),
        ))
    } else {
        None
    };
    let mut artifacts = vec![(
        "baseline".to_string(),
        MODEL_VERSION.to_string(),
        baseline_json,
        input_hash.clone(),
        None,
        base_hash,
        x_json.clone(),
        serde_json::to_string(&prepared.baseline).unwrap(),
    )];
    if let Some(a) = smooth_art {
        artifacts.push(a);
    }

    let st = state.lock().unwrap();
    let job = match st
        .db
        .claim_or_get_job(&fp, req.dataset_id, scheme_id)
    {
        Ok(j) => j,
        Err(e) => return json_err(500, &e.to_string()),
    };
    // If racing against an identical job that already published, replay.
    if job.status == "done" {
        if let Ok(Some((_, report))) = st.db.get_fit_for_job(&fp) {
            return json_ok(json!({"fingerprint": fp, "replayed": true,
                "report": serde_json::from_str::<serde_json::Value>(&report).unwrap()}));
        }
    }
    if let Err(e) = st.db.publish_fit(
        job.id,
        &report_json,
        &canonical_json(&req.spec.baseline),
        prepared.report.converged,
        FITTER_VERSION,
        &artifacts,
    ) {
        return json_err(500, &e.to_string());
    }

    json_ok(json!({
        "fingerprint": fp,
        "scheme_id": scheme_id,
        "replayed": false,
        "accepted": false,
        "converged": prepared.report.converged,
        "auto_candidate": req.auto_candidate,
        "report": prepared.report,
        "baseline": prepared.baseline,
        "smoothed": prepared.smoothed,
        "mask": prepared.mask,
        "corrected": prepared.corrected,
    }))
}
