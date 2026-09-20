#![allow(dead_code)]
use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command};

pub struct ServerGuard {
    pub child: Child,
    pub addr: String,
    db: String,
}

impl Drop for ServerGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        for ext in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{}", self.db, ext));
        }
    }
}

pub fn start_server(port: u16) -> ServerGuard {
    // Ask the OS for a free port by binding then releasing.
    let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = probe.local_addr().unwrap().port();
    drop(probe);
    let db = format!("test_{port}");
    let addr = format!("127.0.0.1:{port}");
    let child = Command::new(env!("CARGO_BIN_EXE_spectra-lab"))
        .args(["--listen", &addr, "--db", &format!("{db}.sqlite")])
        .spawn()
        .expect("start server");
    let guard = ServerGuard {
        child,
        addr: addr.clone(),
        db: format!("{db}.sqlite"),
    };
    // wait for readiness
    for _ in 0..50 {
        if std::net::TcpStream::connect(&addr).is_ok() {
            return guard;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    panic!("server did not start");
}

pub fn http(addr: &str, method: &str, path: &str, body: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(addr).unwrap();
    let req = if body.is_empty() {
        format!("{method} {path} HTTP/1.0\r\nHost: x\r\n\r\n")
    } else {
        format!(
            "{method} {path} HTTP/1.0\r\nHost: x\r\nContent-Type: application/json\r\n\
             Content-Length: {}\r\n\r\n{body}",
            body.len()
        )
    };
    stream.write_all(req.as_bytes()).unwrap();
    let _ = stream.flush();
    let mut all = Vec::new();
    let mut buf = [0u8; 65536];
    loop {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => all.extend_from_slice(&buf[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset
                || e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => panic!("read: {e}"),
        }
    }
    let resp = String::from_utf8_lossy(&all);
    let split = resp.find("\r\n\r\n").expect("headers");
    let code = resp[9..12].parse().unwrap();
    (code, resp[split + 4..].to_string())
}

pub fn post(addr: &str, path: &str, v: serde_json::Value) -> (u16, serde_json::Value) {
    let (c, b) = http(addr, "POST", path, &v.to_string());
    (c, serde_json::from_str(&b).unwrap())
}

pub fn get(addr: &str, path: &str) -> (u16, serde_json::Value) {
    let (c, b) = http(addr, "GET", path, "");
    (c, serde_json::from_str(&b).unwrap())
}
