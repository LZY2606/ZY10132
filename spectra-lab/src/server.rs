//! Minimal HTTP/1.1 server: static files plus a JSON API.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;
const MAX_BODY: usize = 32 * 1024 * 1024;

pub type Handler = Box<dyn Fn(&str, &str) -> (u16, String, String) + Send + Sync>;

pub struct Server {
    listener: TcpListener,
    routes: HashMap<&'static str, std::sync::Arc<Handler>>,
    index_html: String,
}

impl Server {
    pub fn bind(addr: &str) -> std::io::Result<Server> {
        let listener = TcpListener::bind(addr)?;
        Ok(Server {
            listener,
            routes: HashMap::new(),
            index_html: String::new(),
        })
    }

    pub fn local_addr(&self) -> std::io::Result<std::net::SocketAddr> {
        self.listener.local_addr()
    }

    pub fn route(&mut self, path: &'static str, h: Handler) {
        self.routes.insert(path, std::sync::Arc::new(h));
    }

    pub fn serve_static(mut self, index_html: &str) -> ! {
        self.index_html = index_html.to_string();
        let routes: std::sync::Arc<HashMap<&'static str, std::sync::Arc<Handler>>> =
            std::sync::Arc::new(self.routes.drain().collect());
        let index = std::sync::Arc::new(self.index_html.clone());
        for stream in self.listener.incoming().flatten() {
            let routes = routes.clone();
            let index = index.clone();
            thread::Builder::new()
                .stack_size(4 * 1024 * 1024)
                .spawn(move || handle(stream, &routes, &index))
                .ok();
        }
        unreachable!()
    }
}

fn handle(
    mut stream: TcpStream,
    routes: &HashMap<&'static str, std::sync::Arc<Handler>>,
    index_html: &str,
) {
    stream.set_read_timeout(Some(std::time::Duration::from_secs(10))).ok();
    let mut all = Vec::new();
    let mut tmp = [0u8; 65536];
    loop {
        match stream.read(&mut tmp) {
            Ok(0) => break,
            Ok(n) => {
                all.extend_from_slice(&tmp[..n]);
                if let Some(h) = all.windows(4).position(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&all[..h]);
                    let clen = head
                        .lines()
                        .find_map(|l| {
                            let l = l.to_ascii_lowercase();
                            l.strip_prefix("content-length:")
                                .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                        })
                        .unwrap_or(0);
                    if all.len() >= h + 4 + clen {
                        break;
                    }
                }
                if all.len() > MAX_BODY {
                    return;
                }
            }
            Err(_) => break,
        }
    }
    let req = String::from_utf8_lossy(&all);
    let mut lines = req.lines();
    let request_line = match lines.next() {
        Some(l) => l,
        None => return,
    };
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("GET");
    let raw_path = parts.next().unwrap_or("/");
    let (path, query) = raw_path.split_once('?').unwrap_or((raw_path, ""));

    // JSON body: find header/body separator.
    let body = req.split("\r\n\r\n").nth(1).unwrap_or("").to_string();

    if method == "GET" && (path == "/" || path == "/index.html") {
        respond(
            &mut stream,
            200,
            "text/html; charset=utf-8",
            index_html.as_bytes(),
        );
        return;
    }

    // static assets
    let static_map: &[(&str, &str)] = &[
        ("/app.js", include_str!("../static/app.js")),
        ("/styles.css", include_str!("../static/styles.css")),
    ];
    if method == "GET" {
        for (p, content) in static_map {
            if *p == path {
                let mime = if p.ends_with(".js") {
                    "application/javascript; charset=utf-8"
                } else {
                    "text/css; charset=utf-8"
                };
                respond(&mut stream, 200, mime, content.as_bytes());
                return;
            }
        }
    }

    if method == "OPTIONS" {
        respond(&mut stream, 200, "text/plain", b"");
        return;
    }

    if let Some(h) = routes.get(path) {
        let (code, ctype, payload) = h(query, &body);
        respond(&mut stream, code, &ctype, payload.as_bytes());
    } else {
        respond(
            &mut stream,
            404,
            "application/json",
            br#"{"error":"not_found"}"#,
        );
    }
}

fn respond(stream: &mut TcpStream, code: u16, ctype: &str, body: &[u8]) {
    let reason = match code {
        200 => "OK",
        201 => "Created",
        400 => "Bad Request",
        404 => "Not Found",
        409 => "Conflict",
        500 => "Internal Server Error",
        _ => "OK",
    };
    let head = format!(
        "HTTP/1.1 {code} {reason}\r\nContent-Type: {ctype}\r\n\
         Content-Length: {}\r\nConnection: close\r\nAccess-Control-Allow-Origin: *\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body);
    let _ = stream.flush();
    let _ = stream.shutdown(std::net::Shutdown::Write);
}
