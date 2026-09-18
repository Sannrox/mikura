//! 10⁸ hop count + sum envelope on product `Store` join maps.
//! Throwaway. Dual-read versus log replay. Spark stays unsupported.

use mikura::{
    Aggregate, EvaluateRequest, Hop, LocalCompute, ObjectRecord, ObjectSet, PropertyAcl, Store,
};
use mikura_ingest::BatchIngest;
use std::env;
use std::path::Path;
use std::process::ExitCode;
use std::time::Instant;

struct Scale {
    customers: i64,
    orders: i64,
    shipments: i64,
}

impl Scale {
    fn from_objects(objects: i64) -> Result<Self, String> {
        if objects < 10 {
            return Err("need at least 10 objects".into());
        }
        Ok(Self {
            customers: objects / 100,
            orders: objects / 10,
            shipments: objects - objects / 100 - objects / 10,
        })
    }

    fn total(&self) -> i64 {
        self.customers + self.orders + self.shipments
    }
}

fn rec(kind: &str, key: String, hidden: bool, props: Vec<(&str, String)>) -> ObjectRecord {
    ObjectRecord {
        gen: 1,
        kind: kind.into(),
        key,
        hidden,
        action_id: None,
        props: props.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
    }
}

fn request() -> EvaluateRequest {
    EvaluateRequest {
        root_kind: "Customer".into(),
        hops: vec![
            Hop {
                far_kind: "Order".into(),
                join_property: "customer_id".into(),
                incoming: false,
            },
            Hop {
                far_kind: "Shipment".into(),
                join_property: "order_id".into(),
                incoming: false,
            },
        ],
        sum_kind: "Shipment".into(),
        sum_property: "amount".into(),
        aggregate: Aggregate::CountAndSum,
        acl: PropertyAcl::allow_all(),
        filter: None,
        object_bound: 0,
    }
}

fn rss_bytes() -> u64 {
    let pid = std::process::id();
    let out = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &pid.to_string()])
        .output()
        .ok();
    let Some(out) = out else {
        return 0;
    };
    let text = String::from_utf8_lossy(&out.stdout);
    text.trim()
        .parse::<u64>()
        .map(|kb| kb.saturating_mul(1024))
        .unwrap_or(0)
}

fn ingest(store: &mut Store, scale: &Scale, chunk: usize) -> Result<(), String> {
    let mut batch = Vec::with_capacity(chunk);
    for id in 0..scale.customers {
        batch.push(rec(
            "Customer",
            format!("c{id}"),
            id % 100 == 0,
            vec![("region", region(id))],
        ));
        flush_if_full(store, &mut batch, chunk)?;
    }
    for id in 0..scale.orders {
        batch.push(rec(
            "Order",
            format!("o{id}"),
            id % 100 == 0,
            vec![("customer_id", format!("c{}", id % scale.customers))],
        ));
        flush_if_full(store, &mut batch, chunk)?;
    }
    for id in 0..scale.shipments {
        batch.push(rec(
            "Shipment",
            format!("s{id}"),
            id % 100 == 0,
            vec![
                ("order_id", format!("o{}", id % scale.orders)),
                ("amount", format!("{}", (id % 50) + 1)),
            ],
        ));
        flush_if_full(store, &mut batch, chunk)?;
    }
    if !batch.is_empty() {
        BatchIngest::run(store, std::mem::take(&mut batch))?;
    }
    Ok(())
}

fn flush_if_full(
    store: &mut Store,
    batch: &mut Vec<ObjectRecord>,
    chunk: usize,
) -> Result<(), String> {
    if batch.len() >= chunk {
        let n = batch.len();
        let fsync_before = store.log_fsync_count();
        let t = Instant::now();
        BatchIngest::run(store, std::mem::take(batch))?;
        eprintln!(
            "ingested_chunk={n} commit_ms={} fsync_delta={} rss_bytes={}",
            t.elapsed().as_millis(),
            store.log_fsync_count() - fsync_before,
            rss_bytes()
        );
    }
    Ok(())
}

fn region(id: i64) -> String {
    ["eu", "us", "ap"][(id.rem_euclid(3)) as usize].into()
}

fn sidecar_bytes(log: &Path) -> u64 {
    std::fs::metadata(Store::join_map_path(log))
        .map(|m| m.len())
        .unwrap_or(0)
}

fn log_bytes(log: &Path) -> u64 {
    std::fs::metadata(log).map(|m| m.len()).unwrap_or(0)
}

fn main() -> ExitCode {
    let objects = parse_objects(env::args().skip(1)).unwrap_or(10_000);
    let scale = match Scale::from_objects(objects) {
        Ok(scale) => scale,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::from(2);
        }
    };
    let dir = env::temp_dir().join(format!("mikura-envelope-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    if let Err(error) = std::fs::create_dir_all(&dir) {
        eprintln!("mkdir: {error}");
        return ExitCode::from(1);
    }
    let log = dir.join("objects.mikura");
    let ingest_start = Instant::now();
    let mut store = match Store::create(&log) {
        Ok(store) => store,
        Err(error) => {
            eprintln!("create: {error}");
            return ExitCode::from(1);
        }
    };
    if let Err(error) = ingest(&mut store, &scale, 1_000_000) {
        eprintln!("ingest: {error}");
        let _ = std::fs::remove_dir_all(&dir);
        return ExitCode::from(1);
    }
    let ingest_ms = ingest_start.elapsed().as_millis();
    let ingest_rss = rss_bytes();
    let oss = ObjectSet::new(LocalCompute);
    let query_start = Instant::now();
    let live = match oss.evaluate(&store, &request()) {
        Ok(response) => response,
        Err(error) => {
            eprintln!("evaluate: {error:?}");
            let _ = std::fs::remove_dir_all(&dir);
            return ExitCode::from(1);
        }
    };
    let live_query_ms = query_start.elapsed().as_millis();
    let join_bytes = sidecar_bytes(&log);
    let object_log_bytes = log_bytes(&log);
    drop(store);

    let open_start = Instant::now();
    let reopened = match Store::open(&log) {
        Ok(store) => store,
        Err(error) => {
            eprintln!("open: {error}");
            let _ = std::fs::remove_dir_all(&dir);
            return ExitCode::from(1);
        }
    };
    let open_ms = open_start.elapsed().as_millis();
    let open_rss = rss_bytes();
    let reopen_start = Instant::now();
    let from_sidecar = match oss.evaluate(&reopened, &request()) {
        Ok(response) => response,
        Err(error) => {
            eprintln!("reopen evaluate: {error:?}");
            let _ = std::fs::remove_dir_all(&dir);
            return ExitCode::from(1);
        }
    };
    let sidecar_query_ms = reopen_start.elapsed().as_millis();
    drop(reopened);

    std::fs::remove_file(Store::join_map_path(&log)).ok();
    let replay_start = Instant::now();
    let replayed = match Store::open(&log) {
        Ok(store) => store,
        Err(error) => {
            eprintln!("replay: {error}");
            let _ = std::fs::remove_dir_all(&dir);
            return ExitCode::from(1);
        }
    };
    let replay_ms = replay_start.elapsed().as_millis();
    let from_log = match oss.evaluate(&replayed, &request()) {
        Ok(response) => response,
        Err(error) => {
            eprintln!("replay evaluate: {error:?}");
            let _ = std::fs::remove_dir_all(&dir);
            return ExitCode::from(1);
        }
    };
    drop(replayed);

    let dual_read = live.two_hop_count == from_sidecar.two_hop_count
        && live.sum_amount == from_sidecar.sum_amount
        && live.two_hop_count == from_log.two_hop_count
        && live.sum_amount == from_log.sum_amount
        && live.two_hop_count > 0
        && live.sum_amount > 0;

    println!("spike=011-hundred-million-envelope");
    println!("objects={}", scale.total());
    println!("ingest_ms={ingest_ms}");
    println!("ingest_rss_bytes={ingest_rss}");
    println!("log_bytes={object_log_bytes}");
    println!("join_bytes={join_bytes}");
    println!("live_query_ms={live_query_ms}");
    println!("open_ms={open_ms}");
    println!("open_rss_bytes={open_rss}");
    println!("sidecar_query_ms={sidecar_query_ms}");
    println!("replay_ms={replay_ms}");
    println!("two_hop_count={}", from_sidecar.two_hop_count);
    println!("sum_amount={}", from_sidecar.sum_amount);
    println!("dual_read_hold={dual_read}");

    let _ = std::fs::remove_dir_all(&dir);
    if dual_read {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

fn parse_objects(mut args: impl Iterator<Item = String>) -> Option<i64> {
    while let Some(arg) = args.next() {
        if arg == "--objects" {
            return args.next()?.parse().ok();
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_fixture_dual_read_holds_and_hidden_out() {
        let dir = std::env::temp_dir().join("mikura-envelope-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("objects.mikura");
        let scale = Scale::from_objects(1_000).unwrap();
        let mut store = Store::create(&log).unwrap();
        ingest(&mut store, &scale, 200).unwrap();
        assert!(!store.joins().is_visible("Customer", "c0"));
        assert!(!store.joins().is_visible("Order", "o0"));
        assert!(!store.joins().is_visible("Shipment", "s0"));
        let oss = ObjectSet::new(LocalCompute);
        let live = oss.evaluate(&store, &request()).unwrap();
        assert!(live.two_hop_count > 0);
        assert!(live.sum_amount > 0);
        drop(store);
        let reopened = Store::open(&log).unwrap();
        let from_maps = oss.evaluate(&reopened, &request()).unwrap();
        assert_eq!(from_maps.two_hop_count, live.two_hop_count);
        assert_eq!(from_maps.sum_amount, live.sum_amount);
        std::fs::remove_file(Store::join_map_path(&log)).unwrap();
        let replayed = Store::open(&log).unwrap();
        let from_log = oss.evaluate(&replayed, &request()).unwrap();
        assert_eq!(from_log.two_hop_count, live.two_hop_count);
        assert_eq!(from_log.sum_amount, live.sum_amount);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
