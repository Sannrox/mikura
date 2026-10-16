//! Single-process host: ingest, evaluate, load, apply_action, apply_overlay,
//! and hide. `--bearer` arms the RPC envelope on any bind; non-loopback bind
//! still requires a clerk bearer. The `backup`, `verify`, and `restore`
//! subcommands run beside a serving host (ADR 0037).

use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use mikura::backup::{backup_log, backup_log_incremental, restore_log, verify_log, RestorePoint};
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
    let mut argv = std::env::args().skip(1).peekable();
    if let Some(command) = argv.peek().cloned() {
        if matches!(command.as_str(), "backup" | "verify" | "restore") {
            argv.next();
            return run_backup_command(&command, argv);
        }
    }
    let args = match parse_args()? {
        Some(args) => args,
        None => {
            println!(
                "mikura-host {} {}\nusage: mikura-host --log PATH [--bind ADDR] [--stream-bound N] [--request-bound N] [--request-timeout-ms N] [--bearer TOKEN]\n       mikura-host backup --log SRC --to DST [--incremental]\n       mikura-host verify --log PATH\n       mikura-host restore --from BACKUP --to DST [--point ID]\nclose stdin to stop; uncommitted stream pushes are not flushed",
                env!("MIKURA_GIT_VERSION"),
                env!("MIKURA_GIT_COMMIT")
            );
            return Ok(());
        }
    };
    let (host, listener) = Host::listen(
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
    let running = Arc::new(AtomicBool::new(true));
    watch_stdin_then_stop(bound, running.clone());
    host.serve_while(listener, || running.load(Ordering::SeqCst))
}

/// Closing stdin stops the listener without flushing the stream buffer.
fn watch_stdin_then_stop(bound: SocketAddr, running: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        let mut stdin = io::stdin();
        let mut buf = [0u8; 32];
        loop {
            match stdin.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
        }
        running.store(false, Ordering::SeqCst);
        let mut addr = bound;
        if addr.ip().is_unspecified() {
            addr.set_ip(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST));
        }
        let _ = TcpStream::connect_timeout(&addr, Duration::from_secs(2));
    });
}

fn parse_args() -> Result<Option<Args>, String> {
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
            "--help" | "-h" => return Ok(None),
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    let log = log.ok_or_else(|| "missing --log PATH".to_string())?;
    Ok(Some(Args {
        log,
        bind,
        stream_bound,
        request_bound,
        request_timeout_ms,
        bearer,
    }))
}

/// `backup --log SRC --to DST [--incremental]`, `verify --log PATH`, and
/// `restore --from BACKUP --to DST [--point ID]`. Each prints the restore
/// point it reached.
fn run_backup_command(command: &str, args: impl Iterator<Item = String>) -> Result<(), String> {
    let mut log = None;
    let mut from = None;
    let mut to = None;
    let mut point = None;
    let mut incremental = false;
    let mut args = args;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--log" => log = Some(PathBuf::from(required_value("--log", args.next())?)),
            "--from" => from = Some(PathBuf::from(required_value("--from", args.next())?)),
            "--to" => to = Some(PathBuf::from(required_value("--to", args.next())?)),
            "--point" => {
                point = Some(required_value("--point", args.next())?.parse::<RestorePoint>()?)
            }
            "--incremental" => incremental = true,
            other => return Err(format!("unknown argument for {command}: {other}")),
        }
    }
    let missing = |flag: &str| format!("{command}: missing {flag}");
    let reached = match command {
        "backup" => {
            let (src, dst) = (
                log.ok_or_else(|| missing("--log"))?,
                to.ok_or_else(|| missing("--to"))?,
            );
            if incremental {
                backup_log_incremental(&src, &dst)?
            } else {
                backup_log(&src, &dst)?
            }
        }
        "verify" => {
            let report = verify_log(&log.ok_or_else(|| missing("--log"))?)?;
            println!("records {}", report.records);
            report.restore_point
        }
        _ => restore_log(
            &from.ok_or_else(|| missing("--from"))?,
            &to.ok_or_else(|| missing("--to"))?,
            point,
        )?,
    };
    println!("restore point {reached}");
    Ok(())
}

fn required_value(flag: &str, value: Option<String>) -> Result<String, String> {
    value.ok_or_else(|| format!("missing value for {flag}"))
}
