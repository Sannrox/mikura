//! Helpers shared by the `product` and `takeover` bench binaries: the
//! machine class a result belongs to, and a spawned `mikura-host` process
//! spoken to over its JSON-line socket.

use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::SocketAddr;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

/// Results compare only within one machine class.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Machine {
    pub os: String,
    pub arch: String,
    pub cpus: usize,
    pub cpu: String,
}

pub fn machine() -> Machine {
    let cpu = Command::new("sysctl")
        .args(["-n", "machdep.cpu.brand_string"])
        .output()
        .ok()
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .filter(|name| !name.is_empty())
        .or_else(|| {
            std::fs::read_to_string("/proc/cpuinfo")
                .ok()
                .and_then(|info| {
                    info.lines()
                        .find_map(|line| line.strip_prefix("model name"))
                        .map(|rest| rest.trim_start_matches([' ', '\t', ':']).to_string())
                })
        })
        .unwrap_or_else(|| "unknown".into());
    Machine {
        os: std::env::consts::OS.into(),
        arch: std::env::consts::ARCH.into(),
        cpus: std::thread::available_parallelism().map_or(1, usize::from),
        cpu,
    }
}

/// One JSON-line request on a fresh connection. Transport failures are
/// `Err`; a refused request is `Ok` with `ok == false`.
pub fn host_rpc(
    addr: SocketAddr,
    body: &serde_json::Value,
) -> Result<mikura_host::HostResponse, String> {
    let mut client = std::net::TcpStream::connect(addr).map_err(|err| format!("connect: {err}"))?;
    client
        .set_read_timeout(Some(Duration::from_secs(30)))
        .map_err(|err| err.to_string())?;
    client
        .set_write_timeout(Some(Duration::from_secs(30)))
        .map_err(|err| err.to_string())?;
    let line = serde_json::to_string(body).map_err(|err| err.to_string())?;
    client
        .write_all(line.as_bytes())
        .and_then(|_| client.write_all(b"\n"))
        .map_err(|err| format!("write: {err}"))?;
    let mut buf = String::new();
    client
        .read_to_string(&mut buf)
        .map_err(|err| format!("read: {err}"))?;
    serde_json::from_str(buf.trim()).map_err(|err| format!("decode: {err}"))
}

/// Start `mikura-host --log <log> <extra>` on an ephemeral loopback port.
/// Stdin stays piped, so the host serves until killed or the child drops.
pub fn spawn_host(log: &Path, extra: &[&str]) -> Result<(Child, SocketAddr), String> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_mikura-host"))
        .args([
            "--log",
            log.to_str().ok_or("log path is not utf-8")?,
            "--bind",
            "127.0.0.1:0",
            "--stream-bound",
            "8",
        ])
        .args(extra)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| format!("spawn mikura-host: {err}"))?;
    let stdout = child.stdout.take().ok_or("host stdout")?;
    let mut line = String::new();
    BufReader::new(stdout)
        .read_line(&mut line)
        .map_err(|err| format!("read listening line: {err}"))?;
    let addr = line
        .strip_prefix("listening ")
        .map(str::trim)
        .and_then(|value| value.parse().ok());
    match addr {
        Some(addr) => Ok((child, addr)),
        None => {
            let _ = child.kill();
            let mut stderr = String::new();
            if let Some(mut pipe) = child.stderr.take() {
                let _ = pipe.read_to_string(&mut stderr);
            }
            let _ = child.wait();
            Err(format!(
                "expected listening address, got {line:?}: {}",
                stderr.trim()
            ))
        }
    }
}
