//! spectra-lab: auditable local spectroscopy baseline & peak fitting.
#![allow(dead_code)]

mod api;
mod data;
mod db;
mod fit;
mod linalg;
mod model;
mod rng;
mod scheme;
mod server;
mod units;

use std::sync::{Arc, Mutex};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut listen = "127.0.0.1:5332".to_string();
    let mut db_path = "spectra-lab.sqlite".to_string();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--listen" => {
                i += 1;
                listen = args.get(i).cloned().expect("need --listen value");
            }
            "--db" => {
                i += 1;
                db_path = args.get(i).cloned().expect("need --db value");
            }
            "-h" | "--help" => {
                println!("spectra-lab --listen 127.0.0.1:5332 [--db path.sqlite]");
                return;
            }
            other => {
                eprintln!("unknown argument: {other}");
                std::process::exit(2);
            }
        }
        i += 1;
    }

    let db = db::Db::open(&db_path).expect("open database");
    if let Err(e) = db.reset_stale_running() {
        eprintln!("warning: stale job reset failed: {e}");
    }
    let state = Arc::new(Mutex::new(api::AppState { db }));

    let mut srv = server::Server::bind(&listen).expect("bind listen address");
    macro_rules! route {
        ($path:expr, $f:expr) => {{
            let st = state.clone();
            srv.route($path, Box::new(move |q, b| $f(&st, q, b)));
        }};
    }
    route!("/api/datasets", api::datasets);
    route!("/api/upload", api::upload);
    route!("/api/convert", api::convert_endpoint);
    route!("/api/schemes", api::list_schemes);
    route!("/api/schemes/create", api::create_scheme);
    route!("/api/schemes/accept", api::accept_scheme);
    route!("/api/fit", api::run_fit);

    println!("spectra-lab listening on http://{listen}");
    println!("database: {db_path}");
    srv.serve_static(include_str!("../static/index.html"));
}
