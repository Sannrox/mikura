# ADR 0001: Paged object log with group commit

- Status: accepted
- Date: 2026-09-15
- Related: spikes 003–005

## Decision

The kura `Store` log is 4KiB CRC32 pages. Rebuild reads only
`1..=committed_pages` from the superblock. Writers fsync a group of data
pages, then the superblock (default group 32). JSONL is not the product
format.

Uncommitted extra pages are not authority. Missing committed pages fail
closed. A later format bump is a new ADR.
