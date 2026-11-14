# Spike 021: set combinators versus `and`/`or`/`not`

Question ([#338](https://github.com/Sannrox/mikura/issues/338)): is there a
consumer query whose expected answers are ambiguous without set union,
intersection, or difference, and if so, what are the identity,
restriction-binding, and pagination semantics of combined sets?

Set combinators fail closed today
([ADR 0015](../../docs/decisions/0015-composable-object-sets.md),
[ADR 0018](../../docs/decisions/0018-aggregation-semantics.md)).
[#122](https://github.com/Sannrox/mikura/issues/122) closed them because
the two-object seed is unambiguous without them. This run reopens that
question on the product-loop Explorer fixture
([ADR 0016](../../docs/decisions/0016-production-workload.md)).

Published seed queries (three): `load` `component/svc-api`; `evaluate`
exact-match `tier=prod`; incoming hop `incident.affects` → `component`.
None of those answers is a union, intersection, or difference of two
object sets. Same-kind combinations that look like set algebra are
already `and` / `or` / `not` on the current kind: difference of two
equality matches is `and(eq, not eq)`; union of two equality matches is
`or`. Mixed-kind union (`component` ∪ `incident`) is not a product
operator; host `op: union` already fails closed.

Hardware: Linux x86_64, 12 CPUs, AMD Ryzen 5 PRO 4650U, 15 GiB class.
`rustc` stable, `cargo run --release`. Unique temp files include process
id plus a suffix. Resident memory is Linux `VmHWM`.

```text
cd spikes/021-set-combinators
cargo test
cargo run --release -- --objects 10000
cargo run --release -- --objects 100000
```

| n | eq `tier=prod` | `and(eq, not eq)` | `or(eq, eq)` | hop | decoded difference | eq | and | or | hop |
| ---: | ---: | ---: | ---: | ---: | ---: | --- | --- | --- | --- |
| 10⁴ | 0.134 ms | 46.0 ms | 1.36 ms | 2.88 ms | 1.84 ms | HOLD | HOLD | HOLD | HOLD |
| 10⁵ | 1.87 ms | 489 ms | 14.7 ms | 45.8 ms | 24.8 ms | HOLD | HOLD | HOLD | HOLD |

Dual-read on every timed query: product `and(eq tier=prod, not eq
name=billing-api)` keys match a decoded difference of the two `eq`
evaluates. `or` versus a decoded union is covered in tests. Published
combinator query count: **0**.

## Verdict: HOLD `and`/`or`/`not`; no set combinators

- The published Explorer answers stay exact-match `tier` and hop
  `affects`. Inventing a mixed-kind union would add an operator the
  seed does not need.
- Same-kind union, intersection, and difference of predicates are
  already `or`, `and`, and `and`+`not`. Dual-read held.
- Mixed-kind union stays fail closed. Host `predicate.op=union` already
  fails closed in the product suite.
- A synthetic `and`+`not` scan that nears 500 ms at 10⁵ is not an
  engine pick and is not a published query.

Decision: [ADR 0053](../../docs/decisions/0053-set-combinators-stay-absent.md).
This spike does not change `src/`. Revisit when the consumer publishes a
query whose expected answer is a union, intersection, or difference of
two object sets.
