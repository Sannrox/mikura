# Security policy

mikura is pre-1.0 (`0.1.x`, `publish = false`). Security work targets current
`main`. Older snapshots do not receive separate support.

## Reporting a vulnerability

Report vulnerabilities **privately** to the repository maintainers. If this
repository is hosted on GitHub, use GitHub's private vulnerability reporting
on the Security tab.

Do not open a public issue or pull request for an exploitable vulnerability.

Include:

- affected commit or version
- steps to reproduce
- expected impact
- whether local object logs or network exposure are involved

Do not include real credentials or unredacted sensitive data.

## What this crate actually enforces today

These behaviors are implemented and fail closed:

- checksum mismatch on a committed log page
- missing pages inside the committed range
- property ACL denial on the requested aggregate property

These are **not** a security boundary yet:

- `PropertyAcl` is an in-process deny list of `(kind, property)`. There is no
  principal, session, or wire ACL.
- There is no authentication, authorization service, or multi-tenant isolation
  in this crate.
- Object logs on disk are not encrypted by mikura.

Treat a mikura log file as sensitive application data. Do not commit `*.mikura`
files or copy them into issues.

## Safe defaults

- Keep development logs under a temp directory or gitignored `data/`.
- Do not expose a mikura process on a non-loopback address until hosted gRPC
  and on-the-wire ACL exist ([ROADMAP.md](ROADMAP.md)).
