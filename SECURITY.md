# Security policy

mikura is pre-1.0 (`0.1.x`, `publish = false`). Security work targets current
`main`. Older snapshots do not receive separate support.

## Reporting a vulnerability

Report vulnerabilities **privately** via GitHub's private vulnerability
reporting: open the
[Security tab](https://github.com/Sannrox/mikura/security/advisories/new)
and click **"Report a vulnerability"**.

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
- load omits denied properties rather than inventing values

These are **not** a security boundary yet:

- `PropertyAcl` is a request deny list of `(kind, property)`. There is no
  principal, session, or compiled policy.
- Non-loopback bind checks a clerk-owned bearer for equality ([ADR 0007](docs/decisions/0007-host-bearer.md)).
  That is a process secret, not an authorization service or multi-tenant
  isolation. Loopback is unauthenticated unless `--bearer` is set.
- Object logs on disk are not encrypted by mikura.

Treat a mikura log file as sensitive application data. Do not commit `*.mikura`
files or copy them into issues.

## Safe defaults

- Keep development logs under a temp directory or gitignored `data/`.
- Do not expose a mikura process on a non-loopback address until a
  clerk-owned bearer is configured ([ADR 0007](docs/decisions/0007-host-bearer.md)).
  Loopback remains the safe default.
