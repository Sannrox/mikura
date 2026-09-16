//! Loopback ingest/evaluate process. Non-loopback bind is refused.

use std::io::{self, Write};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;

use mikura_host::Host;

struct Args {
    log: PathBuf,
    bind: SocketAddr,
    stream_bound: usize,
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
    let listener = Host::bind(args.bind)?;
    let bound = listener
        .local_addr()
        .map_err(|err| format!("listener address: {err}"))?;
    println!("listening {bound}");
    io::stdout()
        .flush()
        .map_err(|err| format!("flush listen address: {err}"))?;
    let mut host = Host::open(&args.log, args.stream_bound)?;
    host.serve(listener)
}

fn parse_args() -> Result<Args, String> {
    let mut log = None;
    let mut bind: SocketAddr = "127.0.0.1:0"
        .parse()
        .expect("default loopback bind is valid");
    let mut stream_bound = 8;
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
            "--help" | "-h" => {
                return Err(
                    "usage: mikura-host --log PATH [--bind ADDR] [--stream-bound N]".into(),
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
    })
}

fn required_value(flag: &str, value: Option<String>) -> Result<String, String> {
    value.ok_or_else(|| format!("missing value for {flag}"))
}
