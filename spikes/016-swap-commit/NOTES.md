# Spike 016: swap-committed manifest versus restore-from-log

Question ([#335](https://github.com/Sannrox/mikura/issues/335)): the consumer
published an availability target restore-from-log cannot meet (acknowledged
writes survive machine or disk loss). Which contract supersedes
[ADR 0023](../../docs/decisions/0023-single-process-availability.md), and does
its commit envelope break the current write path?

Hardware: Linux x86_64, 12 CPUs, AMD Ryzen 5 PRO 4650U, 15 GiB.
`rustc` stable, `cargo run --release`. Scratch files live on the worktree
disk (`spikes/scratch/`, btrfs SSD). `/tmp` is tmpfs and is not used.
Unique directories include process id plus a suffix.

```text
cd spikes/016-swap-commit
cargo test
cargo run --release -- --mode all --objects 1000
cargo run --release -- --mode reopen --checkpoint-bytes 775946240
```

`--rtt-ms` defaults to 10 and is injected once per sequential storage call
(put, get, compare-and-swap). It stands in for a remote object store. No
vendor client.

## Today's restore path

Product `Store` plus `backup_log` (ADR 0037). 1000 acknowledged appends,
copy the log, 50 more acknowledged appends, delete the live file (disk
loss), open the backup.

| | Result |
| --- | --- |
| Present after restore | 1000 |
| Missing acknowledged writes | **50** (every commit after the backup) |
| Backup | 29 ms |
| Open backup | 15 ms |

Process crash without disk loss still keeps committed pages (existing host
e2e `process_kill_during_writes_keeps_committed_history`). Disk loss RPO is
the last backup copy, not the last `Store::commit`. That is the miss ADR
0023 waits for.

## Swap-commit drills

Throwaway file backend: write+fsync a temp object, then `hard_link` to the
final name (`EEXIST` is the fence), then fsync the directory so the new
name survives power loss. Acknowledgement is the successful
manifest link. A crash during the temp write cannot become the highest
manifest.

| Drill | Result |
| --- | --- |
| Put then SIGKILL before swap | unacked payload absent after reopen |
| SIGKILL after swap | both acked payloads present |
| Second writer at the same version | first swap wins; displaced write returns `fenced` |

`cargo test` covers in-process fault injection and a child-process SIGKILL
(`tests/kill.rs`). Zero acknowledged-write loss on the kill-after-swap
path. Fencing is the swap, not a lease.

## Commit envelope (100 samples, one 4 KiB payload)

| Path | p50 | p95 | p99 | commits/s |
| --- | ---: | ---: | ---: | ---: |
| Product `Store::append` (fsync superblock) | 5.32 ms | 10.7 ms | 22.0 ms | 168 |
| Swap file (put + CAS, object fsync then directory fsync) | 3.68 ms | 5.35 ms | 6.25 ms | 258 |
| Swap remote (same + 10 ms RTT × 2 waves) | 26.0 ms | 29.0 ms | 30.0 ms | 38 |
| Sync follower (leader put, follower put, follower CAS, leader CAS; 10 ms RTT) | 55.9 ms | 88.8 ms | 104 ms | 16.4 |

The comparison that matters is **same machine, same disk**. File swap
matches local fsync (directory fsync included). The baseline writes one
`ObjectRecord` through `Store::append`; the swap path writes a raw 4 KiB
payload. Remote swap is 26 ms p50 at 10 ms injected RTT (about 5× the
5.32 ms fsync baseline; no vendor client). Batch and group commit already
amortize many records into one acknowledgement. The product-loop
publishes no write SLO, so 26 ms is not a published miss.

## Follower prototype (#283-close design)

Synchronous shipping pays **more** than the remote swap (four sequential
calls vs two) and still needs a second stateful copy, lag, and promotion.
Asynchronous shipping: an acknowledged write that is not copied is absent
on the follower (lost acked = 1). The drill commits twice and ships once.

## Reopen (manifest + checkpoint object)

Storage-level open: one get of the manifest, one get of the named
checkpoint. The blob is incompressible (xorshift fill). Page cache
dropped with `posix_fadvise(DONTNEED)` after the write. Product
`JoinMaps` decode is [#340](https://github.com/Sannrox/mikura/issues/340).

| Checkpoint | Write | Reopen (read checkpoint) |
| ---: | ---: | ---: |
| 74 MiB (10^6 log class) | 79 ms | 60 ms |
| 740 MiB (10^7 log class) | 557 ms | 850 ms |

Historical product `Store::open` at 10^7 is ~12–15.5 s of projection
decode ([m10-replication-availability.md](../../docs/plans/m10-replication-availability.md)).
The swap layout's object get is not that bound. Follow-up [#340](https://github.com/Sannrox/mikura/issues/340)
still has to keep decode inside existing recovery.

## Verdict: VALIDATED

Question: which contract meets the published target without breaking the
write path?

Evidence: restore RPO is backup lag; process-kill and fence drills hold;
file swap 3.7 ms vs fsync 5.3 ms; remote swap 26 ms at 10 ms injected RTT;
sync follower slower; async follower loses an already-acked write; 740 MiB
checkpoint object get 850 ms after dropping the page cache. Product
projection decode is [#340](https://github.com/Sannrox/mikura/issues/340).

What worked: one conditional write as acknowledgement and fence. The
`file` backend is the same protocol.

What failed: restore-from-log on disk loss; async shipping.

Recommendation: accept [ADR 0047](../../docs/decisions/0047-object-storage-store-of-record.md).
Do not build followers. Implementation is [#340](https://github.com/Sannrox/mikura/issues/340).
Takeover RTO at 10⁷ and a remote object GET stay unmeasured
([#340](https://github.com/Sannrox/mikura/issues/340),
[#360](https://github.com/Sannrox/mikura/issues/360)). Still throwaway.
No engine in `src/`.
