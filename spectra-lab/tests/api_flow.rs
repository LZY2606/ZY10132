mod common;

use common::{get, post, start_server};
use serde_json::json;

fn spec_v(center: f64, width: f64, fix: bool) -> serde_json::Value {
    json!({
        "smooth_sigma": 0.5,
        "baseline": {"degree": 1, "windows": [[380.0, 470.0], [650.0, 720.0]]},
        "excluded_x": [],
        "peaks": [{
            "kind": "gauss", "center0": center, "width0": width,
            "amp0": 1.0, "fix_center": fix
        }, {
            "kind": "gauss", "center0": 520.0, "width0": 9.0,
            "amp0": 0.6, "fix_center": false
        }, {
            "kind": "lorentz", "center0": 610.0, "width0": 14.0,
            "amp0": 0.5, "fix_center": false
        }]
    })
}

#[test]
fn synthetic_upload_fit_replay_and_immutability() {
    let g = start_server(5401);
    let addr = g.addr.clone();

    let (code, up) = post(&addr, "/api/upload", json!({"synthetic": true}));
    assert_eq!(code, 200);
    let did = up["dataset_id"].as_i64().unwrap();
    assert_eq!(up["n_points"].as_i64().unwrap(), 900);
    assert!(up["diagnostics"]["duplicate_x"].as_array().unwrap().is_empty());

    // First fit
    let (c1, f1) = post(
        &addr,
        "/api/fit",
        json!({"dataset_id": did, "spec": spec_v(498.0, 9.0, false)}),
    );
    assert_eq!(c1, 200, "{f1}");
    assert_eq!(f1["replayed"], false);
    assert!(f1["report"]["converged"].as_bool().unwrap(), "{}", f1["report"]["status"]);
    assert!(f1["report"]["peaks"][0]["area_ci95"][0].as_f64().unwrap() > 0.0);
    let fp1 = f1["fingerprint"].as_str().unwrap().to_string();

    // Same inputs/initials/constraints -> dedup replay, identical fingerprint
    let (c2, f2) = post(
        &addr,
        "/api/fit",
        json!({"dataset_id": did, "spec": spec_v(498.0, 9.0, false)}),
    );
    assert_eq!(c2, 200);
    assert_eq!(f2["replayed"], true);
    assert_eq!(f2["fingerprint"].as_str().unwrap(), fp1);

    // Locking the centre is a NEW scheme/fingerprint; the old one remains.
    let (c3, f3) = post(
        &addr,
        "/api/fit",
        json!({"dataset_id": did, "spec": spec_v(498.0, 9.0, true)}),
    );
    assert_eq!(c3, 200);
    assert_ne!(f3["fingerprint"].as_str().unwrap(), fp1);

    // Multiple schemes coexist.
    let (_, sc) = get(&addr, &format!("/api/schemes?dataset_id={did}"));
    assert!(sc["schemes"].as_array().unwrap().len() >= 2, "schemes {sc}");

    // Auto candidate never auto-accepts; explicit promotion needs the flag.
    let (_, ac) = post(
        &addr,
        "/api/fit",
        json!({"dataset_id": did, "spec": spec_v(500.0, 8.0, false),
                "auto_candidate": true}),
    );
    assert_eq!(ac["accepted"], false);
    let sid = ac["scheme_id"].as_i64().unwrap();
    let (cac0, _) = post(
        &addr,
        "/api/schemes/accept",
        json!({"scheme_id": sid, "accepted": true}),
    );
    assert_eq!(cac0, 409, "auto candidate must not be promoted silently");
    let (cacc, _) = post(
        &addr,
        "/api/schemes/accept",
        json!({"scheme_id": sid, "accepted": true,
                "confirm_promote_auto": true}),
    );
    assert_eq!(cacc, 200);

    // index page
    let (code, _) = common::http(&addr, "GET", "/", "");
    assert_eq!(code, 200);
}

#[test]
fn unit_conversion_preserves_ranges() {
    let g = start_server(5402);
    let addr = g.addr.clone();
    let (code, v) = post(
        &addr,
        "/api/convert",
        json!({
            "from": "nm", "to": "eV",
            "intervals": [[500.0, 510.0]],
            "peaks": [[505.0, 2.0]]
        }),
    );
    assert_eq!(code, 200, "convert response: {v}");
    let iv = v["intervals"][0].as_array().unwrap();
    assert!(iv[0].as_f64().unwrap() < iv[1].as_f64().unwrap());
    // centre of 505 nm = 1239.841984/505 eV
    let c = v["peaks"][0][0].as_f64().unwrap();
    assert!((c - 1239.841_984_392_919_6 / 505.0).abs() < 1e-9);
}

#[test]
fn bad_inputs_are_diagnosed_not_crashing() {
    let g = start_server(5403);
    let addr = g.addr.clone();
    let csv = "w,y\n1,1\n2,2\n2,3\n3,nan";
    let (code, v) = post(&addr, "/api/upload", json!({"csv": csv}));
    assert_eq!(code, 200);
    assert_eq!(v["diagnostics"]["nonfinite_rows"][0], 5);
    assert_eq!(v["diagnostics"]["duplicate_x"][0], 2.0);
    assert_eq!(v["diagnostics"]["too_few_rows"], true);

    // fitting a too-small dataset is refused, not stored as accepted.
    let did = v["dataset_id"].as_i64().unwrap();
    let (code, f) = post(
        &addr,
        "/api/fit",
        json!({"dataset_id": did,
            "spec": {"baseline": {"degree": 0, "windows": [[0.0, 9.0]]},
                     "peaks": [{"kind":"gauss","center0":2.0,"width0":1.0,
                                "amp0":1.0,"fix_center":false}]}}),
    );
    assert_eq!(code, 400);
    assert!(f["error"].as_str().unwrap().contains("too few"));
}

#[test]
fn nonconverged_fit_is_reported_not_accepted() {
    let g = start_server(5404);
    let addr = g.addr.clone();
    let (_, up) = post(&addr, "/api/upload", json!({"synthetic": true}));
    let did = up["dataset_id"].as_i64().unwrap();

    // A single narrow, height-locked-by-log gaussian cannot model three
    // peaks; with a deliberately tiny width the LM reaches its step limit.
    let (code, f) = post(
        &addr,
        "/api/fit",
        json!({"dataset_id": did, "spec": {
            "smooth_sigma": 0.0,
            "baseline": {"degree": 0, "windows": [[380.0, 720.0]]},
            "excluded_x": [],
            "peaks": [
                {"kind": "gauss", "center0": 500.0, "width0": 0.0001,
                 "amp0": 0.0001, "fix_center": true}
            ]
        }}),
    );
    assert_eq!(code, 200);
    // Either it converges poorly or reports non-convergence; in every case
    // it must never be auto-accepted.
    assert_eq!(f["accepted"], false);
    assert_eq!(f["auto_candidate"], false);
    // The last finite iterate + trace are retained for diagnosis.
    let trace = f["report"]["trace"].as_array().unwrap();
    assert!(!trace.is_empty());
    if !f["report"]["converged"].as_bool().unwrap() {
        assert_ne!(f["report"]["status"], "converged");
    }
}
