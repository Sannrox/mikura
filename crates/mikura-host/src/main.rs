//! Loopback ingest/evaluate process. `--bearer` arms the RPC envelope on any
//! bind; non-loopback bind still requires a clerk bearer.

use std::io::{self, Write};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use mikura_host::{Host, DEFAULT_REQUEST_BOUND, DEFAULT_REQUEST_TIMEOUT_MS};

struct Args {
    log: PathBuf,
    bind: SocketAddr,
    stream_bound: usize,
    request_bound: usize,
    request_timeout_ms: u64,
    bearer: Option<String>,
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let args = parse_args()?;
    let (mut host, listener) = Host::listen(
        &args.log,
        args.stream_bound,
        args.bind,
        args.bearer.as_deref(),
    )?;
    host.set_request_limits(
        args.request_bound,
        Duration::from_millis(args.request_timeout_ms),
    )?;
    let bound = listener
        .local_addr()
        .map_err(|err| format!("listener address: {err}"))?;
    println!("listening {bound}");
    io::stdout()
        .flush()
        .map_err(|err| format!("flush listen address: {err}"))?;
    host.serve(listener)
}

fn parse_args() -> Result<Args, String> {
    let mut log = None;
    let mut bind: SocketAddr = "127.0.0.1:0"
        .parse()
        .expect("default loopback bind is valid");
    let mut stream_bound = 8;
    let mut request_bound = DEFAULT_REQUEST_BOUND;
    let mut request_timeout_ms = DEFAULT_REQUEST_TIMEOUT_MS;
    let mut bearer = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--log" => {
                log = Some(PathBuf::from(required_value("--log", args.next())?));
            }
            "--bind" => {
                let value = required_value("--bind", args.next())?;
                bind = value
                    .parse()
                    .map_err(|err| format!("invalid --bind {value}: {err}"))?;
            }
            "--stream-bound" => {
                let value = required_value("--stream-bound", args.next())?;
                stream_bound = value
                    .parse()
                    .map_err(|err| format!("invalid --stream-bound {value}: {err}"))?;
            }
            "--request-bound" => {
                let value = required_value("--request-bound", args.next())?;
                request_bound = value
                    .parse()
                    .map_err(|err| format!("invalid --request-bound {value}: {err}"))?;
            }
            "--request-timeout-ms" => {
                let value = required_value("--request-timeout-ms", args.next())?;
                request_timeout_ms = value
                    .parse()
                    .map_err(|err| format!("invalid --request-timeout-ms {value}: {err}"))?;
            }
            "--bearer" => {
                bearer = Some(required_value("--bearer", args.next())?);
            }
            "--help" | "-h" => {
                return Err(
                    "usage: mikura-host --log PATH [--bind ADDR] [--stream-bound N] [--request-bound N] [--request-timeout-ms N] [--bearer TOKEN]"
                        .into(),
                );
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    let log = log.ok_or_else(|| "missing --log PATH".to_string())?;
    Ok(Args {
        log,
        bind,
        stream_bound,
        request_bound,
        request_timeout_ms,
        bearer,
    })
}

fn required_value(flag: &str, value: Option<String>) -> Result<String, String> {
    value.ok_or_else(|| format!("missing value for {flag}"))
}
