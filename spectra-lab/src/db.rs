//! SQLite persistence.
//!
//! Publication rule: a fit is written in a single transaction that inserts
//! the job row, the fit row and the derived artifacts together.  A crash
//! can therefore never leave "half a parameter set" visible; an unfinished
//! job is simply retried and the job key (content hash) dedupes the work.

use rusqlite::{params, Connection, OpenFlags};
use std::sync::Mutex;

pub const SCHEMA_VERSION: i64 = 1;

pub struct Db {
    conn: Mutex<Connection>,
}

pub struct JobHit {
    pub id: i64,
    pub status: String,
    pub fit_id: Option<i64>,
}

const SCHEMA: &str = r#"
PRAGMA journal_mode=WAL;
CREATE TABLE IF NOT EXISTS schema_meta (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS datasets (
  id INTEGER PRIMARY KEY,
  input_hash TEXT NOT NULL UNIQUE,
  x_unit TEXT NOT NULL,
  y_kind TEXT NOT NULL,
  x_header TEXT NOT NULL,
  y_header TEXT NOT NULL,
  raw_csv TEXT NOT NULL,
  n_points INTEGER NOT NULL,
  loader_version TEXT NOT NULL,
  diagnostics_json TEXT NOT NULL,
  source TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE TABLE IF NOT EXISTS schemes (
  id INTEGER PRIMARY KEY,
  dataset_id INTEGER NOT NULL REFERENCES datasets(id),
  parent_id INTEGER REFERENCES schemes(id),
  spec_json TEXT NOT NULL,
  fingerprint TEXT NOT NULL,
  origin TEXT NOT NULL,              -- manual | auto_candidate
  accepted INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE TABLE IF NOT EXISTS jobs (
  id INTEGER PRIMARY KEY,
  fingerprint TEXT NOT NULL UNIQUE,
  dataset_id INTEGER NOT NULL,
  scheme_id INTEGER NOT NULL,
  status TEXT NOT NULL,              -- queued | running | done | failed
  fit_id INTEGER,
  attempts INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL DEFAULT (datetime('now')),
  updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE TABLE IF NOT EXISTS fits (
  id INTEGER PRIMARY KEY,
  job_id INTEGER NOT NULL UNIQUE REFERENCES jobs(id),
  report_json TEXT NOT NULL,
  baseline_json TEXT NOT NULL,
  converged INTEGER NOT NULL,
  accepted INTEGER NOT NULL DEFAULT 0,
  fitter_version TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE TABLE IF NOT EXISTS artifacts (
  id INTEGER PRIMARY KEY,
  dataset_id INTEGER NOT NULL REFERENCES datasets(id),
  kind TEXT NOT NULL,                -- baseline | smoothed
  method_version TEXT NOT NULL,
  params_json TEXT NOT NULL,
  input_hash TEXT NOT NULL,
  upstream_hash TEXT,
  artifact_hash TEXT NOT NULL UNIQUE,
  x_json TEXT NOT NULL,
  y_json TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
"#;

impl Db {
    pub fn open(path: &str) -> rusqlite::Result<Db> {
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_URI,
        )?;
        conn.pragma_update(None, "busy_timeout", 10_000)?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.execute_batch(SCHEMA)?;
        conn.execute(
            "INSERT INTO schema_meta(key,value) VALUES('schema_version',?1)
             ON CONFLICT(key) DO UPDATE SET value=?1",
            params![SCHEMA_VERSION.to_string()],
        )?;
        Ok(Db {
            conn: Mutex::new(conn),
        })
    }

    /// In-memory database (tests).
    pub fn open_memory() -> rusqlite::Result<Db> {
        let conn = Connection::open_in_memory()?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.execute_batch(SCHEMA)?;
        Ok(Db {
            conn: Mutex::new(conn),
        })
    }

    pub fn insert_dataset(
        &self,
        input_hash: &str,
        x_unit: &str,
        y_kind: &str,
        x_header: &str,
        y_header: &str,
        raw_csv: &str,
        n_points: usize,
        loader_version: &str,
        diagnostics_json: &str,
        source: &str,
    ) -> rusqlite::Result<i64> {
        let c = self.conn.lock().unwrap();
        c.execute(
            "INSERT INTO datasets(input_hash,x_unit,y_kind,x_header,y_header,
             raw_csv,n_points,loader_version,diagnostics_json,source)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)
             ON CONFLICT(input_hash) DO NOTHING",
            params![
                input_hash,
                x_unit,
                y_kind,
                x_header,
                y_header,
                raw_csv,
                n_points as i64,
                loader_version,
                diagnostics_json,
                source
            ],
        )?;
        c.query_row(
            "SELECT id FROM datasets WHERE input_hash=?1",
            params![input_hash],
            |r| r.get(0),
        )
    }

    pub fn get_dataset(&self, id: i64) -> rusqlite::Result<Option<String>> {
        let c = self.conn.lock().unwrap();
        let mut q = c.prepare("SELECT raw_csv FROM datasets WHERE id=?1")?;
        let mut rows = q.query(params![id])?;
        match rows.next()? {
            Some(r) => Ok(Some(r.get(0)?)),
            None => Ok(None),
        }
    }

    pub fn list_datasets(&self) -> rusqlite::Result<Vec<(i64, String, String, i64, String)>> {
        let c = self.conn.lock().unwrap();
        let mut q = c.prepare(
            "SELECT id,input_hash,x_unit,n_points,created_at FROM datasets ORDER BY id",
        )?;
        let rows = q.query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })?;
        rows.collect()
    }

    pub fn insert_scheme(
        &self,
        dataset_id: i64,
        parent_id: Option<i64>,
        spec_json: &str,
        fingerprint: &str,
        origin: &str,
    ) -> rusqlite::Result<i64> {
        let c = self.conn.lock().unwrap();
        c.execute(
            "INSERT INTO schemes(dataset_id,parent_id,spec_json,fingerprint,origin)
             VALUES(?1,?2,?3,?4,?5)",
            params![dataset_id, parent_id, spec_json, fingerprint, origin],
        )?;
        Ok(c.last_insert_rowid())
    }

    pub fn set_scheme_accepted(&self, id: i64, accepted: bool) -> rusqlite::Result<()> {
        let c = self.conn.lock().unwrap();
        // only one accepted scheme per dataset
        let ds: i64 = c.query_row(
            "SELECT dataset_id FROM schemes WHERE id=?1",
            params![id],
            |r| r.get(0),
        )?;
        let tx = c.unchecked_transaction()?;
        tx.execute("UPDATE schemes SET accepted=0 WHERE dataset_id=?1", params![ds])?;
        tx.execute(
            "UPDATE schemes SET accepted=?2 WHERE id=?1",
            params![id, accepted as i64],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn scheme_origin(&self, id: i64) -> rusqlite::Result<Option<String>> {
        let c = self.conn.lock().unwrap();
        let mut q = c.prepare("SELECT origin FROM schemes WHERE id=?1")?;
        let mut rows = q.query(params![id])?;
        match rows.next()? {
            Some(r) => Ok(Some(r.get(0)?)),
            None => Ok(None),
        }
    }

    pub fn list_schemes(
        &self,
        dataset_id: i64,
    ) -> rusqlite::Result<Vec<(i64, String, String, i64, Option<i64>, String)>> {
        let c = self.conn.lock().unwrap();
        let mut q = c.prepare(
            "SELECT id,spec_json,origin,accepted,parent_id,fingerprint FROM schemes
             WHERE dataset_id=?1 ORDER BY id",
        )?;
        let rows = q.query_map(params![dataset_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?))
        })?;
        rows.collect()
    }

    /// Atomically claim a job.  Creates the job row if the fingerprint is
    /// new, returns the existing row otherwise (dedup).  `running` jobs left
    /// behind by a crash are reset to queued once by the caller.
    pub fn claim_or_get_job(
        &self,
        fingerprint: &str,
        dataset_id: i64,
        scheme_id: i64,
    ) -> rusqlite::Result<JobHit> {
        let c = self.conn.lock().unwrap();
        c.execute(
            "INSERT INTO jobs(fingerprint,dataset_id,scheme_id,status)
             VALUES(?1,?2,?3,'queued') ON CONFLICT(fingerprint) DO NOTHING",
            params![fingerprint, dataset_id, scheme_id],
        )?;
        c.query_row(
            "SELECT id,status,fit_id FROM jobs WHERE fingerprint=?1",
            params![fingerprint],
            |r| {
                Ok(JobHit {
                    id: r.get(0)?,
                    status: r.get(1)?,
                    fit_id: r.get(2)?,
                })
            },
        )
    }

    pub fn reset_stale_running(&self) -> rusqlite::Result<usize> {
        let c = self.conn.lock().unwrap();
        c.execute(
            "UPDATE jobs SET status='queued', attempts=attempts+1
             WHERE status='running'",
            [],
        )
    }

    /// Publish job + fit + artifacts atomically.
    pub fn publish_fit(
        &self,
        job_id: i64,
        report_json: &str,
        baseline_json: &str,
        converged: bool,
        fitter_version: &str,
        artifacts: &[(String, String, String, String, Option<String>, String, String, String)],
    ) -> rusqlite::Result<i64> {
        let mut c = self.conn.lock().unwrap();
        let tx = c.transaction()?;
        tx.execute(
            "UPDATE jobs SET status='done', fit_id=NULL,
             updated_at=datetime('now') WHERE id=?1",
            params![job_id],
        )?;
        tx.execute(
            "INSERT INTO fits(job_id,report_json,baseline_json,converged,
             fitter_version) VALUES(?1,?2,?3,?4,?5)",
            params![job_id, report_json, baseline_json, converged as i64, fitter_version],
        )?;
        let fit_id = tx.last_insert_rowid();
        tx.execute(
            "UPDATE jobs SET fit_id=?2 WHERE id=?1",
            params![job_id, fit_id],
        )?;
        for (kind, mv, pj, input_hash, upstream, ah, xj, yj) in artifacts {
            tx.execute(
                "INSERT OR IGNORE INTO artifacts(dataset_id,kind,method_version,params_json,
                 input_hash,upstream_hash,artifact_hash,x_json,y_json)
                 SELECT dataset_id,?2,?3,?4,?5,?6,?7,?8,?9 FROM jobs WHERE id=?1",
                params![
                    job_id,
                    kind,
                    mv,
                    pj,
                    input_hash,
                    upstream,
                    ah,
                    xj,
                    yj
                ],
            )?;
        }
        tx.commit()?;
        Ok(fit_id)
    }

    pub fn get_fit_for_job(&self, fingerprint: &str) -> rusqlite::Result<Option<(i64, String)>> {
        let c = self.conn.lock().unwrap();
        let mut q = c.prepare(
            "SELECT f.id,f.report_json FROM fits f
             JOIN jobs j ON j.fit_id=f.id WHERE j.fingerprint=?1",
        )?;
        let mut rows = q.query(params![fingerprint])?;
        match rows.next()? {
            Some(r) => Ok(Some((r.get(0)?, r.get(1)?))),
            None => Ok(None),
        }
    }
}
