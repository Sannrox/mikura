//! HTTP object backend (ADR 0047 §12).
//!
//! PUT uses `If-None-Match: *` as the exclusive create. Errors map to
//! [`StorageError`] so callers never see a transport dump. Credentials and
//! the endpoint come from the caller; this module holds neither default.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use super::{valid_key, StorageCalls, StorageError};

pub(crate) struct HttpStore {
    endpoint: String,
    token: Option<String>,
    puts: AtomicU64,
    gets: AtomicU64,
    cas: AtomicU64,
    current: AtomicU64,
    collect: AtomicU64,
    probe: AtomicU64,
    lens: AtomicU64,
}

impl HttpStore {
    pub(crate) fn connect(endpoint: &str, token: Option<String>) -> Result<Self, StorageError> {
        Ok(Self {
            endpoint: endpoint.trim_end_matches('/').to_string(),
            token,
            puts: AtomicU64::new(0),
            gets: AtomicU64::new(0),
            cas: AtomicU64::new(0),
            current: AtomicU64::new(0),
            collect: AtomicU64::new(0),
            probe: AtomicU64::new(0),
            lens: AtomicU64::new(0),
        })
    }

    pub(crate) fn storage_calls(&self) -> StorageCalls {
        StorageCalls {
            puts: self.puts.load(Ordering::Relaxed),
            gets: self.gets.load(Ordering::Relaxed),
            cas: self.cas.load(Ordering::Relaxed),
            current: self.current.load(Ordering::Relaxed),
            collect: self.collect.load(Ordering::Relaxed),
            probe: self.probe.load(Ordering::Relaxed),
            object_len: self.lens.load(Ordering::Relaxed),
        }
    }

    pub(crate) fn probe(&self) -> Result<u64, StorageError> {
        self.probe.fetch_add(1, Ordering::Relaxed);
        let key = "probe";
        self.put(key, b"a")?;
        match self.put(key, b"b") {
            Err(error) if error.is_precondition() => {}
            Ok(_) => {
                let _ = self.remove(key);
                return Err(StorageError::unavailable(
                    "conditional write probe failed: both competing puts succeeded",
                ));
            }
            Err(error) => {
                let _ = self.remove(key);
                return Err(error);
            }
        }
        self.remove(key)?;
        Ok(0)
    }

    pub(crate) fn put(&self, key: &str, bytes: &[u8]) -> Result<u64, StorageError> {
        self.puts.fetch_add(1, Ordering::Relaxed);
        self.put_exclusive(key, bytes)
    }

    fn put_exclusive(&self, key: &str, bytes: &[u8]) -> Result<u64, StorageError> {
        if !http_safe_key(key) {
            return Err(StorageError::unavailable("bad object key"));
        }
        self.call("PUT", &format!("/{key}"), &[("If-None-Match", "*")], bytes)
            .map(|_| 0)
    }

    pub(crate) fn get(&self, key: &str) -> Result<Vec<u8>, StorageError> {
        self.gets.fetch_add(1, Ordering::Relaxed);
        if !http_safe_key(key) {
            return Err(StorageError::unavailable("bad object key"));
        }
        self.call("GET", &format!("/{key}"), &[], b"")
    }

    pub(crate) fn object_len(&self, key: &str) -> Result<u64, StorageError> {
        self.lens.fetch_add(1, Ordering::Relaxed);
        if !http_safe_key(key) {
            return Err(StorageError::unavailable("bad object key"));
        }
        http_head_len(&self.endpoint, self.token.as_deref(), key)
    }

    pub(crate) fn cas(&self, expected: u64, bytes: &[u8]) -> Result<(u64, u64), StorageError> {
        self.cas.fetch_add(1, Ordering::Relaxed);
        let next = expected + 1;
        let key = format!("manifest.{next}");
        match self.put_exclusive(&key, bytes) {
            Ok(fsyncs) => Ok((next, fsyncs)),
            Err(error) if error.is_precondition() => Err(StorageError::precondition(key)),
            Err(error) => self.resolve_cas(expected, next, bytes, error),
        }
    }

    fn resolve_cas(
        &self,
        expected: u64,
        next: u64,
        bytes: &[u8],
        error: StorageError,
    ) -> Result<(u64, u64), StorageError> {
        match self.get(&format!("manifest.{next}")) {
            Ok(landed) if landed == bytes => Ok((next, 0)),
            Ok(_) => Err(StorageError::precondition(format!("manifest.{next}"))),
            Err(get_err) if get_err.is_not_found() => match self.current() {
                Ok(Some((version, _))) if version > expected => {
                    Err(StorageError::precondition(format!("manifest.{next}")))
                }
                Ok(_) => Err(error),
                Err(current_err) => Err(StorageError::unknown_outcome(format!(
                    "{error}; {current_err}"
                ))),
            },
            Err(get_err) => Err(StorageError::unknown_outcome(format!("{error}; {get_err}"))),
        }
    }

    pub(crate) fn current(&self) -> Result<Option<(u64, Vec<u8>)>, StorageError> {
        self.current.fetch_add(1, Ordering::Relaxed);
        let mut highest = None;
        for name in self.list()? {
            if let Some(rest) = name.strip_prefix("manifest.") {
                if let Ok(version) = rest.parse::<u64>() {
                    if highest.is_none_or(|v| version > v) {
                        highest = Some(version);
                    }
                }
            }
        }
        match highest {
            Some(version) => {
                let key = format!("manifest.{version}");
                Ok(Some((version, self.get(&key)?)))
            }
            None => Ok(None),
        }
    }

    pub(crate) fn has_durable_objects(&self) -> Result<bool, StorageError> {
        Ok(self.list()?.iter().any(|name| !name.starts_with('.')))
    }

    pub(crate) fn collect(
        &self,
        live: &std::collections::BTreeSet<String>,
        fenced_before: u64,
        still_owner: &dyn Fn() -> Result<bool, String>,
    ) -> Result<(), StorageError> {
        self.collect.fetch_add(1, Ordering::Relaxed);
        let mut names = self.list()?;
        names.sort_by(|a, b| super::collect_order(a, b));
        for name in names {
            if !super::collectable(&name, live, fenced_before) {
                continue;
            }
            match still_owner() {
                Ok(true) => {}
                Ok(false) => {
                    return Err(StorageError::unavailable(format!(
                        "{}: lost maintenance ownership",
                        crate::LOG_FENCED
                    )));
                }
                Err(err) => return Err(StorageError::unavailable(err)),
            }
            self.remove(&name)?;
        }
        Ok(())
    }

    pub(crate) fn list(&self) -> Result<Vec<String>, StorageError> {
        let bytes = self.call("GET", "/?list=1", &[], b"")?;
        let text = String::from_utf8(bytes)
            .map_err(|_| StorageError::unavailable("list: object names are not utf-8"))?;
        Ok(text
            .lines()
            .map(str::trim)
            .filter(|line| http_safe_key(line))
            .map(str::to_string)
            .collect())
    }

    fn remove(&self, key: &str) -> Result<(), StorageError> {
        if !http_safe_key(key) {
            return Err(StorageError::unavailable("bad object key"));
        }
        match self.call("DELETE", &format!("/{key}"), &[], b"") {
            Ok(_) => Ok(()),
            Err(error) if error.is_not_found() => Ok(()),
            Err(error) => Err(error),
        }
    }

    fn call(
        &self,
        method: &str,
        path: &str,
        extra: &[(&str, &str)],
        body: &[u8],
    ) -> Result<Vec<u8>, StorageError> {
        http_call(
            &self.endpoint,
            self.token.as_deref(),
            method,
            path,
            extra,
            body,
        )
    }
}

struct HttpReply {
    code: u16,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

fn http_call(
    endpoint: &str,
    token: Option<&str>,
    method: &str,
    path: &str,
    extra: &[(&str, &str)],
    body: &[u8],
) -> Result<Vec<u8>, StorageError> {
    let reply = http_exchange(endpoint, token, method, path, extra, body, true)?;
    map_http_code(reply.code, method, path)?;
    Ok(reply.body)
}

fn http_head_len(endpoint: &str, token: Option<&str>, key: &str) -> Result<u64, StorageError> {
    let path = format!("/{key}");
    let reply = http_exchange(endpoint, token, "HEAD", &path, &[], b"", false)?;
    map_http_code(reply.code, "HEAD", &path)?;
    reply
        .headers
        .get("content-length")
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| StorageError::unavailable(format!("HEAD /{key}: store unavailable")))
}

fn map_http_code(code: u16, method: &str, path: &str) -> Result<(), StorageError> {
    match code {
        200 | 201 | 204 => Ok(()),
        404 => Err(StorageError::not_found(path.trim_start_matches('/'))),
        412 => Err(StorageError::precondition(path.trim_start_matches('/'))),
        0 if method == "PUT" => Err(StorageError::unknown_outcome(format!(
            "{method} {path}: store unavailable"
        ))),
        0 => Err(StorageError::unavailable(format!(
            "{method} {path}: store unavailable"
        ))),
        other => Err(StorageError::unavailable(format!(
            "{method} {path}: http {other}"
        ))),
    }
}

fn http_exchange(
    endpoint: &str,
    token: Option<&str>,
    method: &str,
    path: &str,
    extra: &[(&str, &str)],
    body: &[u8],
    read_body: bool,
) -> Result<HttpReply, StorageError> {
    let unavailable = || StorageError::unavailable(format!("{method} {path}: store unavailable"));
    let after_send = || {
        if method == "PUT" {
            StorageError::unknown_outcome(format!("{method} {path}: store unavailable"))
        } else {
            unavailable()
        }
    };
    let (host, addr) = http_addr(endpoint)?;
    let mut stream =
        TcpStream::connect_timeout(&addr, Duration::from_secs(5)).map_err(|_| unavailable())?;
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .map_err(|_| unavailable())?;
    stream
        .set_write_timeout(Some(Duration::from_secs(30)))
        .map_err(|_| unavailable())?;
    let mut request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {host}\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    if let Some(token) = token {
        request.push_str(&format!("Authorization: Bearer {token}\r\n"));
    }
    for (name, value) in extra {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str("\r\n");
    stream
        .write_all(request.as_bytes())
        .and_then(|_| stream.write_all(body))
        .map_err(|_| after_send())?;
    let mut reader = BufReader::new(stream);
    let mut status_line = String::new();
    reader
        .read_line(&mut status_line)
        .map_err(|_| after_send())?;
    let code = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
        .unwrap_or(0);
    let mut headers = BTreeMap::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).map_err(|_| after_send())?;
        if line == "\r\n" || line == "\n" || line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(
                name.trim().to_ascii_lowercase(),
                value.trim().trim_end_matches('\r').to_string(),
            );
        }
    }
    if headers
        .get("transfer-encoding")
        .is_some_and(|value| value.to_ascii_lowercase().contains("chunked"))
    {
        return Err(after_send());
    }
    let body = if !read_body {
        Vec::new()
    } else {
        match headers.get("content-length") {
            Some(value) => {
                let len = value.parse::<usize>().map_err(|_| after_send())?;
                let mut payload = vec![0u8; len];
                if len > 0 {
                    reader.read_exact(&mut payload).map_err(|_| after_send())?;
                }
                payload
            }
            None => {
                let mut payload = Vec::new();
                reader.read_to_end(&mut payload).map_err(|_| after_send())?;
                payload
            }
        }
    };
    Ok(HttpReply {
        code,
        headers,
        body,
    })
}

fn http_safe_key(key: &str) -> bool {
    valid_key(key)
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-')
}

fn http_addr(endpoint: &str) -> Result<(String, SocketAddr), StorageError> {
    let rest = endpoint
        .strip_prefix("http://")
        .ok_or_else(|| StorageError::unavailable("object endpoint must be an http URL"))?;
    if rest.contains('/') {
        return Err(StorageError::unavailable(
            "object endpoint must be an http host:port with no path",
        ));
    }
    let addr: SocketAddr = rest
        .parse()
        .map_err(|_| StorageError::unavailable("object endpoint host is not a socket address"))?;
    Ok((rest.to_string(), addr))
}

/// Loopback HTTP object namespace for tests and the host probe drill.
///
/// Not a production backend. `exclusive` is the conditional-write proof:
/// a store that overwrites on the second put fails the writer probe.
pub struct LocalObjectServer {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    objects: Arc<Mutex<BTreeMap<String, Vec<u8>>>>,
    join: Option<JoinHandle<()>>,
}

impl LocalObjectServer {
    /// Exclusive create: a second PUT of the same key is refused.
    pub fn start() -> Result<Self, String> {
        Self::spawn(true)
    }

    /// Overwrite on every PUT. The writer probe must refuse this store.
    pub fn start_weak() -> Result<Self, String> {
        Self::spawn(false)
    }

    fn spawn(exclusive: bool) -> Result<Self, String> {
        let listener = TcpListener::bind("127.0.0.1:0")
            .map_err(|e| format!("bind local object store: {e}"))?;
        let addr = listener
            .local_addr()
            .map_err(|e| format!("local object store address: {e}"))?;
        let stop = Arc::new(AtomicBool::new(false));
        let objects = Arc::new(Mutex::new(BTreeMap::new()));
        let run_stop = Arc::clone(&stop);
        let run_objects = Arc::clone(&objects);
        let join = thread::spawn(move || serve(listener, exclusive, run_stop, run_objects));
        Ok(Self {
            addr,
            stop,
            objects,
            join: Some(join),
        })
    }

    pub fn endpoint(&self) -> String {
        format!("http://{}", self.addr)
    }

    pub fn get(&self, key: &str) -> Option<Vec<u8>> {
        self.objects.lock().ok()?.get(key).cloned()
    }

    /// True when any stored object contains `needle`. Used to prove ciphertext.
    pub fn contains_bytes(&self, needle: &[u8]) -> bool {
        if needle.is_empty() {
            return false;
        }
        self.objects.lock().ok().is_some_and(|map| {
            map.values()
                .any(|bytes| bytes.windows(needle.len()).any(|window| window == needle))
        })
    }
}

impl Drop for LocalObjectServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect_timeout(&self.addr, Duration::from_millis(50));
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

fn serve(
    listener: TcpListener,
    exclusive: bool,
    stop: Arc<AtomicBool>,
    objects: Arc<Mutex<BTreeMap<String, Vec<u8>>>>,
) {
    while !stop.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _)) => {
                if stop.load(Ordering::SeqCst) {
                    break;
                }
                let objects = Arc::clone(&objects);
                thread::spawn(move || {
                    let _ = handle_conn(stream, exclusive, objects);
                });
            }
            Err(_) => break,
        }
    }
}

fn handle_conn(
    stream: TcpStream,
    exclusive: bool,
    objects: Arc<Mutex<BTreeMap<String, Vec<u8>>>>,
) -> Result<(), String> {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(|e| e.to_string())?;
    let mut reader = BufReader::new(stream.try_clone().map_err(|e| e.to_string())?);
    let mut first = String::new();
    reader.read_line(&mut first).map_err(|e| e.to_string())?;
    let mut parts = first.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("/").to_string();
    let mut headers = BTreeMap::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).map_err(|e| e.to_string())?;
        if line == "\r\n" || line == "\n" || line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(
                name.trim().to_ascii_lowercase(),
                value.trim().trim_end_matches('\r').to_string(),
            );
        }
    }
    let len = headers
        .get("content-length")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = vec![0u8; len];
    if len > 0 {
        reader.read_exact(&mut body).map_err(|e| e.to_string())?;
    }
    let dispatch_method = if method == "HEAD" {
        "GET"
    } else {
        method.as_str()
    };
    let (status, payload) = dispatch(dispatch_method, &path, exclusive, &objects, body);
    let mut stream = reader.into_inner();
    let header = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        payload.len()
    );
    stream
        .write_all(header.as_bytes())
        .map_err(|e| e.to_string())?;
    if method != "HEAD" {
        stream.write_all(&payload).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn dispatch(
    method: &str,
    path: &str,
    exclusive: bool,
    objects: &Mutex<BTreeMap<String, Vec<u8>>>,
    body: Vec<u8>,
) -> (&'static str, Vec<u8>) {
    let (path, query) = path.split_once('?').unwrap_or((path, ""));
    let key = path.trim_start_matches('/');
    match method {
        "PUT" => {
            if key.is_empty() || !valid_key(key) {
                return ("400 Bad Request", Vec::new());
            }
            let mut map = match objects.lock() {
                Ok(map) => map,
                Err(_) => return ("503 Service Unavailable", Vec::new()),
            };
            if exclusive && map.contains_key(key) {
                return ("412 Precondition Failed", Vec::new());
            }
            map.insert(key.to_string(), body);
            ("201 Created", Vec::new())
        }
        "GET" if query.contains("list=1") || key.is_empty() => {
            let map = match objects.lock() {
                Ok(map) => map,
                Err(_) => return ("503 Service Unavailable", Vec::new()),
            };
            let mut names: Vec<_> = map.keys().cloned().collect();
            names.sort();
            ("200 OK", names.join("\n").into_bytes())
        }
        "GET" => {
            if key.is_empty() || !valid_key(key) {
                return ("400 Bad Request", Vec::new());
            }
            let map = match objects.lock() {
                Ok(map) => map,
                Err(_) => return ("503 Service Unavailable", Vec::new()),
            };
            match map.get(key) {
                Some(bytes) => ("200 OK", bytes.clone()),
                None => ("404 Not Found", Vec::new()),
            }
        }
        "DELETE" => {
            if key.is_empty() || !valid_key(key) {
                return ("400 Bad Request", Vec::new());
            }
            let mut map = match objects.lock() {
                Ok(map) => map,
                Err(_) => return ("503 Service Unavailable", Vec::new()),
            };
            map.remove(key);
            ("204 No Content", Vec::new())
        }
        _ => ("405 Method Not Allowed", Vec::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn serve_once(response: &'static [u8]) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf);
            let _ = stream.write_all(response);
        });
        format!("http://{addr}")
    }

    #[test]
    fn chunked_response_is_unavailable() {
        let endpoint = serve_once(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n5\r\nhello\r\n0\r\n\r\n",
        );
        let store = HttpStore::connect(&endpoint, None).unwrap();
        let err = store.get("x").unwrap_err();
        assert!(err.to_string().contains("unavailable"), "{err}");
        assert!(!err.to_string().contains("TcpStream"), "{err}");
    }

    #[test]
    fn missing_content_length_reads_to_eof() {
        let endpoint = serve_once(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\nhello");
        let store = HttpStore::connect(&endpoint, None).unwrap();
        assert_eq!(store.get("x").unwrap(), b"hello");
    }

    #[test]
    fn head_without_content_length_is_unavailable() {
        let endpoint = serve_once(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n");
        let store = HttpStore::connect(&endpoint, None).unwrap();
        let err = store.object_len("x").unwrap_err();
        assert!(err.to_string().contains("unavailable"), "{err}");
    }

    #[test]
    fn put_without_a_response_is_unknown_outcome() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf);
        });
        let store = HttpStore::connect(&format!("http://{addr}"), None).unwrap();
        let err = store.put("x", b"a").unwrap_err();
        assert_eq!(err.kind_name(), "unknown outcome", "{err}");
        assert!(!err.to_string().contains("TcpStream"), "{err}");
    }
}
