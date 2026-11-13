//! Throwaway: does a swap-committed manifest meet the #335 availability
//! target without breaking the current fsync commit envelope?
//!
//! Not the mikura store of record. Object names are spike-only.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use mikura::backup::backup_log;
use mikura::{ObjectRecord, Store};

#[cfg(unix)]
extern "C" {
    fn posix_fadvise(fd: i32, offset: i64, len: i64, advice: i32) -> i32;
}

#[cfg(unix)]
const POSIX_FADV_DONTNEED: i32 = 4;

const PAGE: usize = 4096;
const NS: &str = "bench";
const SAMPLES: usize = 100;
static NEXT_SEG: AtomicU64 = AtomicU64::new(1);

fn main() {
    let mode = arg_str("--mode", "all");
    let result = match mode.as_str() {
        "writer-child" => writer_child(),
        "inspect" => inspect_dir(),
        _ => {
            let rtt_ms = arg_u64("--rtt-ms", 10);
            let checkpoint_bytes = arg_u64("--checkpoint-bytes", 0);
            let objects = arg_u64("--objects", 1_000);
            run(&mode, rtt_ms, checkpoint_bytes, objects)
        }
    };
    match result {
        Ok(()) => {}
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    }
}

fn run(mode: &str, rtt_ms: u64, checkpoint_bytes: u64, objects: u64) -> Result<(), String> {
    let machine = machine_class();
    println!("machine_os={}", machine.0);
    println!("machine_arch={}", machine.1);
    println!("machine_cpus={}", machine.2);
    println!("machine_cpu={}", machine.3);
    println!("rtt_ms={rtt_ms}");

    if mode == "all" || mode == "drills" {
        run_drills()?;
    }
    if mode == "all" || mode == "restore" {
        run_restore(objects)?;
    }
    if mode == "all" || mode == "envelope" {
        run_envelope(rtt_ms)?;
    }
    if mode == "all" || mode == "follower" {
        run_follower(rtt_ms)?;
    }
    if mode == "all" || mode == "reopen" {
        let bytes = if checkpoint_bytes == 0 {
            74 * 1024 * 1024
        } else {
            checkpoint_bytes
        };
        run_reopen(bytes)?;
    }
    Ok(())
}

fn writer_child() -> Result<(), String> {
    let dir = PathBuf::from(arg_str("--dir", ""));
    if dir.as_os_str().is_empty() {
        return Err("--dir required".into());
    }
    let kill = arg_str("--kill", "");
    let store = FileStore::create(&dir)?;
    let mut writer = SwapWriter::open(&store)?;
    match kill.as_str() {
        "after-swap" => {
            writer.commit(b"keep-1")?;
            writer.commit(b"keep-2")?;
            fs::write(dir.join("ready"), b"ok").map_err(|e| e.to_string())?;
            thread::sleep(Duration::from_secs(60));
        }
        "after-put" => {
            writer.commit(b"acked-1")?;
            writer.pause_after_put = true;
            writer.ready_path = Some(dir.join("ready"));
            let _ = writer.commit(b"lost-2");
        }
        other => return Err(format!("unknown --kill {other}")),
    }
    Ok(())
}

fn inspect_dir() -> Result<(), String> {
    let dir = PathBuf::from(arg_str("--dir", ""));
    if dir.as_os_str().is_empty() {
        return Err("--dir required".into());
    }
    let store = FileStore::create(&dir)?;
    let opened = SwapWriter::open(&store)?;
    println!("records={}", opened.len());
    for i in 0..opened.len() {
        let payload = opened.payload(i)?;
        println!("payload_{i}={}", String::from_utf8_lossy(&payload));
    }
    Ok(())
}

fn run_drills() -> Result<(), String> {
    crash_before_swap_loses_unacked()?;
    swap_then_reopen_keeps_acked()?;
    displaced_writer_is_fenced()?;
    println!("drills=pass");
    Ok(())
}

fn crash_before_swap_loses_unacked() -> Result<(), String> {
    let dir = Scratch::new("kill-put");
    let store = FileStore::create(dir.path())?;
    let mut writer = SwapWriter::open(&store)?;
    writer.commit(b"acked-1")?;
    writer.crash_before_swap = true;
    let err = writer
        .commit(b"lost-2")
        .expect_err("put-then-crash must not ack");
    if !err.contains("crash before swap") {
        return Err(err);
    }
    let opened = SwapWriter::open(&store)?;
    if opened.len() != 1 {
        return Err(format!("expected 1 acked record, got {}", opened.len()));
    }
    if opened.payload(0)? != b"acked-1" {
        return Err("replacement served the unacked payload".into());
    }
    Ok(())
}

fn swap_then_reopen_keeps_acked() -> Result<(), String> {
    let dir = Scratch::new("kill-swap");
    let store = FileStore::create(dir.path())?;
    let mut writer = SwapWriter::open(&store)?;
    writer.commit(b"keep-1")?;
    writer.commit(b"keep-2")?;
    let opened = SwapWriter::open(&store)?;
    if opened.len() != 2 {
        return Err(format!("expected 2 acked records, got {}", opened.len()));
    }
    if opened.payload(0)? != b"keep-1" || opened.payload(1)? != b"keep-2" {
        return Err("replacement did not serve both acked payloads".into());
    }
    Ok(())
}

fn displaced_writer_is_fenced() -> Result<(), String> {
    let dir = Scratch::new("fence");
    let store = FileStore::create(dir.path())?;
    let mut a = SwapWriter::open(&store)?;
    a.commit(b"a-1")?;
    let mut b = SwapWriter::open(&store)?;
    b.commit(b"b-2")?;
    let err = a.commit(b"a-3").expect_err("displaced writer must fail");
    if !err.contains("fenced") {
        return Err(err);
    }
    let opened = SwapWriter::open(&store)?;
    if opened.len() != 2 {
        return Err(format!(
            "expected 2 records after fence, got {}",
            opened.len()
        ));
    }
    if opened.payload(1)? != b"b-2" {
        return Err("replacement did not serve the winning writer".into());
    }
    Ok(())
}

fn run_restore(objects: u64) -> Result<(), String> {
    let dir = Scratch::new("restore");
    let live = dir.path().join("live.mikura");
    let backup = dir.path().join("backup.mikura");
    let mut store = Store::create(&live)?;
    for i in 0..objects {
        store.append(item(i))?;
    }
    let backup_started = Instant::now();
    let point = backup_log(&live, &backup)?;
    let backup_ns = elapsed_ns(backup_started);
    let extra = 50u64;
    for i in objects..objects + extra {
        store.append(item(i))?;
    }
    let live_pages = store.committed_pages();
    drop(store);
    fs::remove_file(&live).map_err(|e| e.to_string())?;
    let restore_started = Instant::now();
    let restored = Store::open(&backup)?;
    let restore_ns = elapsed_ns(restore_started);
    let mut present = 0u64;
    let mut missing = 0u64;
    for i in 0..objects + extra {
        match restored.load(
            NS,
            "Item",
            &format!("k{i}"),
            &mikura::PropertyAcl::allow_all(),
        ) {
            Ok(_) => present += 1,
            Err(_) => missing += 1,
        }
    }
    println!("restore_objects={objects}");
    println!("restore_extra_commits={extra}");
    println!("restore_backup_point={}", point.commit_point);
    println!("restore_live_pages_after_extra={live_pages}");
    println!("restore_present={present}");
    println!("restore_missing={missing}");
    println!("restore_rpo_acked_writes={missing}");
    println!("restore_backup_ns={backup_ns}");
    println!("restore_open_ns={restore_ns}");
    if present != objects || missing != extra {
        return Err(format!(
            "disk-loss RPO expected present={objects} missing={extra}, got {present}/{missing}"
        ));
    }
    Ok(())
}

fn run_envelope(rtt_ms: u64) -> Result<(), String> {
    let fsync = measure_fsync_baseline()?;
    let file = measure_swap("file", Duration::ZERO)?;
    let remote = measure_swap("remote", Duration::from_millis(rtt_ms))?;
    print_envelope("fsync_baseline", &fsync);
    print_envelope("swap_file", &file);
    print_envelope("swap_remote", &remote);
    Ok(())
}

struct Envelope {
    p50: u64,
    p95: u64,
    p99: u64,
    throughput_per_s: f64,
}

fn measure_fsync_baseline() -> Result<Envelope, String> {
    let dir = Scratch::new("fsync");
    let log = dir.path().join("objects.mikura");
    let mut store = Store::create(&log)?;
    store.append(item(0))?;
    let mut samples = Vec::with_capacity(SAMPLES);
    for i in 1..=SAMPLES as u64 {
        let started = Instant::now();
        store.append(item(i))?;
        samples.push(elapsed_ns(started));
    }
    Ok(summarize(&mut samples))
}

fn measure_swap(label: &str, rtt: Duration) -> Result<Envelope, String> {
    let dir = Scratch::new(&format!("swap-{label}"));
    let store = FileStore::create(dir.path())?.with_rtt(rtt);
    let mut writer = SwapWriter::open(&store)?;
    writer.commit(b"warm")?;
    let mut samples = Vec::with_capacity(SAMPLES);
    for i in 0..SAMPLES {
        let payload = [i as u8; PAGE];
        let started = Instant::now();
        writer.commit(&payload)?;
        samples.push(elapsed_ns(started));
    }
    Ok(summarize(&mut samples))
}

fn print_envelope(name: &str, env: &Envelope) {
    println!("{name}_p50_ns={}", env.p50);
    println!("{name}_p95_ns={}", env.p95);
    println!("{name}_p99_ns={}", env.p99);
    println!("{name}_commits_per_s={:.1}", env.throughput_per_s);
}

fn run_follower(rtt_ms: u64) -> Result<(), String> {
    let rtt = Duration::from_millis(rtt_ms);
    let sync = measure_follower(true, rtt)?;
    let rpo_async = follower_async_rpo()?;
    print_envelope("follower_sync", &sync);
    println!("follower_async_lost_acked={rpo_async}");
    Ok(())
}

fn measure_follower(sync: bool, rtt: Duration) -> Result<Envelope, String> {
    let leader_dir = Scratch::new("leader");
    let follower_dir = Scratch::new("follower");
    let leader = FileStore::create(leader_dir.path())?.with_rtt(rtt);
    let follower = FileStore::create(follower_dir.path())?.with_rtt(rtt);
    let mut writer = SwapWriter::open(&leader)?;
    writer.follower = Some(follower.clone());
    writer.sync_follower = sync;
    writer.commit(b"warm")?;
    let mut samples = Vec::with_capacity(SAMPLES);
    for i in 0..SAMPLES {
        let payload = [i as u8; 64];
        let started = Instant::now();
        writer.commit(&payload)?;
        samples.push(elapsed_ns(started));
    }
    Ok(summarize(&mut samples))
}

fn follower_async_rpo() -> Result<u64, String> {
    let leader_dir = Scratch::new("async-leader");
    let follower_dir = Scratch::new("async-follower");
    let leader = FileStore::create(leader_dir.path())?;
    let follower = FileStore::create(follower_dir.path())?;
    let mut writer = SwapWriter::open(&leader)?;
    writer.follower = Some(follower.clone());
    writer.sync_follower = false;
    writer.commit(b"shipped")?;
    writer.ship_follower()?;
    writer.commit(b"unshipped")?;
    let leader_opened = SwapWriter::open(&leader)?;
    if leader_opened.len() != 2 {
        return Err(format!(
            "leader expected 2 acked records, got {}",
            leader_opened.len()
        ));
    }
    let opened = SwapWriter::open(&follower)?;
    if opened.len() != 1 {
        return Err(format!(
            "async follower expected 1 shipped record, got {}",
            opened.len()
        ));
    }
    Ok((leader_opened.len() - opened.len()) as u64)
}

fn run_reopen(checkpoint_bytes: u64) -> Result<(), String> {
    let dir = Scratch::new("reopen");
    let store = FileStore::create(dir.path())?;
    let mut writer = SwapWriter::open(&store)?;
    let mut blob = vec![0u8; checkpoint_bytes as usize];
    fill_incompressible(&mut blob);
    let write_started = Instant::now();
    writer.commit_checkpoint(&blob)?;
    let write_ns = elapsed_ns(write_started);
    drop_page_cache(dir.path())?;
    drop(writer);
    let open_started = Instant::now();
    let opened = SwapWriter::open(&store)?;
    let loaded = opened.checkpoint_len();
    let open_ns = elapsed_ns(open_started);
    if loaded != checkpoint_bytes as usize {
        return Err("reopen lost the checkpoint object".into());
    }
    println!("reopen_checkpoint_bytes={checkpoint_bytes}");
    println!("reopen_write_ns={write_ns}");
    println!("reopen_ns={open_ns}");
    Ok(())
}

#[derive(Clone)]
struct FileStore {
    root: PathBuf,
    rtt: Duration,
}

impl FileStore {
    fn create(root: &Path) -> Result<Self, String> {
        fs::create_dir_all(root.join("obj")).map_err(|e| e.to_string())?;
        Ok(Self {
            root: root.to_path_buf(),
            rtt: Duration::ZERO,
        })
    }

    fn with_rtt(mut self, rtt: Duration) -> Self {
        self.rtt = rtt;
        self
    }

    fn delay(&self) {
        if !self.rtt.is_zero() {
            thread::sleep(self.rtt);
        }
    }

    fn put(&self, key: &str, bytes: &[u8]) -> Result<(), String> {
        self.delay();
        self.put_now(key, bytes)
    }

    fn put_now(&self, key: &str, bytes: &[u8]) -> Result<(), String> {
        let tmp_key = format!("{key}.tmp.{}", NEXT_SEG.fetch_add(1, Ordering::Relaxed));
        let tmp = self.root.join("obj").join(&tmp_key);
        let dest = self.root.join("obj").join(key);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
            .map_err(|e| format!("put {key} tmp: {e}"))?;
        file.write_all(&(bytes.len() as u64).to_le_bytes())
            .map_err(|e| e.to_string())?;
        file.write_all(bytes).map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        drop(file);
        match fs::hard_link(&tmp, &dest) {
            Ok(()) => {
                let _ = fs::remove_file(&tmp);
                // Directory fsync publishes the new name across power loss.
                File::open(dest.parent().unwrap_or(Path::new(".")))
                    .and_then(|dir| dir.sync_all())
                    .map_err(|e| format!("put {key} dir: {e}"))?;
                Ok(())
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                let _ = fs::remove_file(&tmp);
                Err(format!("put {key}: already exists"))
            }
            Err(error) => {
                let _ = fs::remove_file(&tmp);
                Err(format!("put {key}: {error}"))
            }
        }
    }

    fn get(&self, key: &str) -> Result<Vec<u8>, String> {
        self.delay();
        self.get_now(key)
    }

    fn get_now(&self, key: &str) -> Result<Vec<u8>, String> {
        let path = self.root.join("obj").join(key);
        let mut file = File::open(&path).map_err(|e| format!("get {key}: {e}"))?;
        let mut len_buf = [0u8; 8];
        file.read_exact(&mut len_buf).map_err(|e| e.to_string())?;
        let len = u64::from_le_bytes(len_buf) as usize;
        let mut bytes = vec![0u8; len];
        file.read_exact(&mut bytes).map_err(|e| e.to_string())?;
        Ok(bytes)
    }

    fn cas(&self, expected: u64, bytes: &[u8]) -> Result<u64, String> {
        self.delay();
        let next = expected + 1;
        let key = format!("manifest.{next}");
        match self.put_now(&key, bytes) {
            Ok(()) => Ok(next),
            Err(error) if error.contains("already exists") => {
                Err(format!("fenced: manifest {next} exists"))
            }
            Err(error) => Err(error),
        }
    }

    fn current_manifest(&self) -> Result<Option<(u64, Vec<u8>)>, String> {
        let mut highest = None;
        let dir = self.root.join("obj");
        for entry in fs::read_dir(&dir).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some(rest) = name.strip_prefix("manifest.") {
                if let Ok(version) = rest.parse::<u64>() {
                    if highest.as_ref().is_none_or(|(v, _)| version > *v) {
                        highest = Some((version, name));
                    }
                }
            }
        }
        match highest {
            Some((version, name)) => Ok(Some((version, self.get(&name)?))),
            None => Ok(None),
        }
    }
}

struct SwapWriter {
    store: FileStore,
    version: u64,
    records: Vec<String>,
    checkpoint: Option<String>,
    crash_before_swap: bool,
    pause_after_put: bool,
    ready_path: Option<PathBuf>,
    follower: Option<FileStore>,
    sync_follower: bool,
}

impl SwapWriter {
    fn open(store: &FileStore) -> Result<Self, String> {
        match store.current_manifest()? {
            None => Ok(Self {
                store: store.clone(),
                version: 0,
                records: Vec::new(),
                checkpoint: None,
                crash_before_swap: false,
                pause_after_put: false,
                ready_path: None,
                follower: None,
                sync_follower: false,
            }),
            Some((version, bytes)) => {
                let (records, checkpoint) = decode_manifest(&bytes)?;
                Ok(Self {
                    store: store.clone(),
                    version,
                    records,
                    checkpoint,
                    crash_before_swap: false,
                    pause_after_put: false,
                    ready_path: None,
                    follower: None,
                    sync_follower: false,
                })
            }
        }
    }

    fn len(&self) -> usize {
        self.records.len()
    }

    fn payload(&self, index: usize) -> Result<Vec<u8>, String> {
        self.store.get(&self.records[index])
    }

    fn checkpoint_len(&self) -> usize {
        self.checkpoint
            .as_ref()
            .and_then(|key| self.store.get(key).ok())
            .map(|bytes| bytes.len())
            .unwrap_or(0)
    }

    fn commit(&mut self, payload: &[u8]) -> Result<u64, String> {
        let key = unique_name("seg");
        self.store.put(&key, payload)?;
        if self.crash_before_swap {
            return Err("crash before swap".into());
        }
        if self.pause_after_put {
            if let Some(path) = &self.ready_path {
                fs::write(path, b"ok").map_err(|e| e.to_string())?;
            }
            thread::sleep(Duration::from_secs(60));
            return Err("pause after put".into());
        }
        let mut next = self.records.clone();
        next.push(key.clone());
        if self.sync_follower {
            self.copy_to_follower(&key, payload)?;
            if let Some(follower) = &self.follower {
                follower.cas(self.version, &encode_manifest(&next, &self.checkpoint))?;
            }
        }
        let version = self
            .store
            .cas(self.version, &encode_manifest(&next, &self.checkpoint))?;
        self.records = next;
        self.version = version;
        Ok(version)
    }

    fn commit_checkpoint(&mut self, payload: &[u8]) -> Result<u64, String> {
        let key = unique_name("ckpt");
        self.store.put(&key, payload)?;
        let version = self.store.cas(
            self.version,
            &encode_manifest(&self.records, &Some(key.clone())),
        )?;
        self.checkpoint = Some(key);
        self.version = version;
        Ok(version)
    }

    fn ship_follower(&self) -> Result<(), String> {
        let Some(follower) = &self.follower else {
            return Ok(());
        };
        for key in &self.records {
            if follower.get(key).is_err() {
                let bytes = self.store.get(key)?;
                follower.put(key, &bytes)?;
            }
        }
        if let Some(key) = &self.checkpoint {
            if follower.get(key).is_err() {
                follower.put(key, &self.store.get(key)?)?;
            }
        }
        let bytes = encode_manifest(&self.records, &self.checkpoint);
        let _ = follower.cas(self.version.saturating_sub(1), &bytes);
        Ok(())
    }

    fn copy_to_follower(&self, key: &str, payload: &[u8]) -> Result<(), String> {
        let Some(follower) = &self.follower else {
            return Ok(());
        };
        follower.put(key, payload)
    }
}

fn encode_manifest(records: &[String], checkpoint: &Option<String>) -> Vec<u8> {
    let mut out = String::new();
    if let Some(key) = checkpoint {
        out.push_str("ckpt=");
        out.push_str(key);
        out.push('\n');
    }
    for key in records {
        out.push_str(key);
        out.push('\n');
    }
    out.into_bytes()
}

fn decode_manifest(bytes: &[u8]) -> Result<(Vec<String>, Option<String>), String> {
    let text = String::from_utf8(bytes.to_vec()).map_err(|e| e.to_string())?;
    let mut records = Vec::new();
    let mut checkpoint = None;
    for line in text.lines() {
        if let Some(key) = line.strip_prefix("ckpt=") {
            checkpoint = Some(key.to_string());
        } else if !line.is_empty() {
            records.push(line.to_string());
        }
    }
    Ok((records, checkpoint))
}

fn unique_name(prefix: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!(
        "{prefix}.{}-{nanos}-{}",
        std::process::id(),
        NEXT_SEG.fetch_add(1, Ordering::Relaxed)
    )
}

fn item(i: u64) -> ObjectRecord {
    ObjectRecord {
        gen: 1,
        namespace: NS.into(),
        kind: "Item".into(),
        key: format!("k{i}"),
        hidden: false,
        action_id: None,
        schema_version: 1,
        props: [("n".into(), i.to_string())].into_iter().collect(),
    }
}

fn summarize(samples: &mut [u64]) -> Envelope {
    samples.sort_unstable();
    let n = samples.len();
    let pct = |p: usize| {
        let i = n.saturating_mul(p) / 100;
        samples[i.min(n.saturating_sub(1))]
    };
    let mean = samples.iter().sum::<u64>() as f64 / n as f64;
    Envelope {
        p50: pct(50),
        p95: pct(95),
        p99: pct(99),
        throughput_per_s: if mean == 0.0 {
            0.0
        } else {
            1_000_000_000.0 / mean
        },
    }
}

struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        // /tmp is tmpfs here; fsync and reopen must hit the worktree disk.
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("scratch")
            .join(format!("016-{}-{}-{}", std::process::id(), label, nanos));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("scratch dir");
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn fill_incompressible(buf: &mut [u8]) {
    let mut state = 0x9E3779B97F4A7C15u64;
    for chunk in buf.chunks_mut(8) {
        state = state
            .wrapping_mul(0xD1B54A32D192ED03)
            .wrapping_add(0x9E3779B97F4A7C15);
        let bytes = state.to_le_bytes();
        chunk.copy_from_slice(&bytes[..chunk.len()]);
    }
}

fn drop_page_cache(root: &Path) -> Result<(), String> {
    let dir = root.join("obj");
    for entry in fs::read_dir(&dir).map_err(|e| e.to_string())? {
        let path = entry.map_err(|e| e.to_string())?.path();
        if !path.is_file() {
            continue;
        }
        let file = File::open(&path).map_err(|e| e.to_string())?;
        let rc = unsafe { posix_fadvise(file.as_raw_fd(), 0, 0, POSIX_FADV_DONTNEED) };
        if rc != 0 {
            return Err(format!("posix_fadvise {}: {rc}", path.display()));
        }
    }
    Ok(())
}

fn elapsed_ns(started: Instant) -> u64 {
    started.elapsed().as_nanos() as u64
}

fn arg_u64(flag: &str, default: u64) -> u64 {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == flag {
            return args.next().and_then(|s| s.parse().ok()).unwrap_or(default);
        }
    }
    default
}

fn arg_str(flag: &str, default: &str) -> String {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == flag {
            return args.next().unwrap_or_else(|| default.to_string());
        }
    }
    default.to_string()
}

fn machine_class() -> (String, String, usize, String) {
    let os = std::env::consts::OS.to_string();
    let arch = std::env::consts::ARCH.to_string();
    let cpus = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    let cpu = std::fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|text| {
            text.lines()
                .find_map(|line| line.strip_prefix("model name"))
                .map(|rest| rest.trim().trim_start_matches(':').trim().to_string())
        })
        .unwrap_or_else(|| "unknown".into());
    (os, arch, cpus, cpu)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn put_without_swap_is_not_acked() {
        crash_before_swap_loses_unacked().unwrap();
    }

    #[test]
    fn swap_is_acked_after_reopen() {
        swap_then_reopen_keeps_acked().unwrap();
    }

    #[test]
    fn later_writer_fences_the_previous() {
        displaced_writer_is_fenced().unwrap();
    }

    #[test]
    fn async_follower_loses_unshipped_ack() {
        assert_eq!(follower_async_rpo().unwrap(), 1);
    }

    #[test]
    fn disk_loss_restore_drops_commits_after_backup() {
        run_restore(20).unwrap();
    }

    #[test]
    fn replacement_writer_can_append_after_reopen() {
        let dir = Scratch::new("takeover");
        let store = FileStore::create(dir.path()).unwrap();
        let mut writer = SwapWriter::open(&store).unwrap();
        writer.commit(b"first").unwrap();
        drop(writer);
        let mut writer = SwapWriter::open(&store).unwrap();
        writer.commit(b"second").unwrap();
        let opened = SwapWriter::open(&store).unwrap();
        assert_eq!(opened.len(), 2);
        assert_eq!(opened.payload(0).unwrap(), b"first");
        assert_eq!(opened.payload(1).unwrap(), b"second");
    }

    #[test]
    fn sync_follower_opens_the_acked_history() {
        let leader_dir = Scratch::new("sync-leader");
        let follower_dir = Scratch::new("sync-follower");
        let leader = FileStore::create(leader_dir.path()).unwrap();
        let follower = FileStore::create(follower_dir.path()).unwrap();
        let mut writer = SwapWriter::open(&leader).unwrap();
        writer.follower = Some(follower.clone());
        writer.sync_follower = true;
        writer.commit(b"one").unwrap();
        writer.commit(b"two").unwrap();
        let opened = SwapWriter::open(&follower).unwrap();
        assert_eq!(opened.len(), 2);
        assert_eq!(opened.payload(0).unwrap(), b"one");
        assert_eq!(opened.payload(1).unwrap(), b"two");
    }
}
