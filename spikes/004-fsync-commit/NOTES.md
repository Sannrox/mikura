# Spike 004: fsync data page, then superblock commit pointer

Question: If we fsync a data page **before** advancing `committed_pages` in
the superblock, is an extra CRC-valid page *not* authority, and does a
missing committed page fail closed?

Hardware: Apple M2 Pro, 32 GiB, Darwin arm64

```text
cargo test
cargo run --release -- --objects 10000 --sync none
cargo run --release -- --objects 10000 --sync page
```

| Policy | snapshot 10⁴ | committed pages | two-hop | rebuild identity |
| --- | ---: | ---: | ---: | --- |
| `--sync none` | 7 ms | 142 | 99 | hold |
| `--sync page` | 1_145 ms | 142 | 99 | hold |

`--sync page` fsyncs the data page, then rewrites and fsyncs the superblock
(~2 fsyncs × 142 pages). About **160×** slower than no-sync on this disk.

`cargo test`:

- Append a sealed empty page **without** bumping `committed_pages` → rebuild
  identity unchanged (orphan page is not authority).
- Truncate below the committed range → `committed pages missing`.
- Flip a byte in a committed page → `checksum mismatch`.
- `Writer::open` continues after the committed end and last-gen wins.

## Verdict: VALIDATED

003 dropped a torn **last** page. This spike adds a **commit pointer**:
rebuild reads only `1..=committed`. Uncommitted bytes, even with a valid
CRC, are not the store of record. Order is write+fsync page, then
write+fsync superblock.

What this is not: group commit, `F_FULLFSYNC` vs `fsync`, WAL + checkpoint,
or a production durability spec. macOS `sync_data` is not a power-loss
proof.

Recommendation: keep the pointer. Next production-shaped step is **group
commit** (fsync a batch of pages, then one superblock) so 10⁷ ingest is not
160× the no-sync path. Still throwaway. Still Rust. No engine pick.
