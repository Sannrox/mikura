use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use std::{fs, thread};

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_mikura-swap-commit-spike"))
}

fn scratch(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("scratch")
        .join(format!(
            "016-kill-{}-{}-{}",
            std::process::id(),
            label,
            nanos
        ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("scratch");
    dir
}

fn wait_ready(path: &Path) {
    let started = Instant::now();
    while !path.exists() {
        assert!(
            started.elapsed() < Duration::from_secs(15),
            "writer child did not become ready"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn inspect(dir: &Path) -> String {
    let out = Command::new(bin())
        .args(["--mode", "inspect", "--dir", dir.to_str().unwrap()])
        .output()
        .expect("inspect");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn kill_writer(dir: &Path, kill: &str) {
    let mut child = Command::new(bin())
        .args([
            "--mode",
            "writer-child",
            "--dir",
            dir.to_str().unwrap(),
            "--kill",
            kill,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn writer-child");
    wait_ready(&dir.join("ready"));
    child.kill().expect("kill writer-child");
    let _ = child.wait();
}

#[test]
fn process_kill_after_swap_keeps_acked_payloads() {
    let dir = scratch("after-swap");
    kill_writer(&dir, "after-swap");
    let text = inspect(&dir);
    assert!(text.contains("records=2"), "{text}");
    assert!(text.contains("payload_0=keep-1"), "{text}");
    assert!(text.contains("payload_1=keep-2"), "{text}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn process_kill_after_put_drops_unacked_payload() {
    let dir = scratch("after-put");
    kill_writer(&dir, "after-put");
    let text = inspect(&dir);
    assert!(text.contains("records=1"), "{text}");
    assert!(text.contains("payload_0=acked-1"), "{text}");
    assert!(!text.contains("lost-2"), "{text}");
    let _ = fs::remove_dir_all(&dir);
}
