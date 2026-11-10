# Test262 baseline

Test262 is the primary broad conformance signal for quickjs-oxide. The suite
revision and the authenticated QuickJS 2026-06-04 patch and configuration are
pinned in [`compat/upstream.toml`](../compat/upstream.toml) and mirrored by
[`dev-support/test262/current.conf`](../dev-support/test262/current.conf).

## Official metrics

<!-- current-test262-metrics:start -->
Metrics are reported in this order:

1. **QuickJS-scope pass:** 82,359 / 84,503 (97.463%). The denominator is
   every variant the pinned QuickJS `test262.conf` runs, and those of the fork's
   features (the profile's `[fork-features]`); it excludes the
   17,534 `skipped-*` variants that QuickJS itself skips.
2. **Full pass:** 82,359 / 102,037 (80.715%). Every frozen Test262 variant is in
   the denominator.
3. **Eligible coverage:** 84,288 / 102,037 (82.605%). This measures how much of
   the full vector the current profile admits to execution.
4. **Runnable pass quality:** 82,359 / 84,288 (97.711%). This diagnoses
   admitted behavior and complements the coverage metrics above.

The frozen outcome summary is:

```text
fail-async=16 fail-async-done-count=53 fail-parse=324 fail-runtime=1536 pass=82359 skipped-config-exclude=6700 skipped-feature=10834 unsupported-module=128 unsupported-negative-provenance=87
```
<!-- current-test262-metrics:end -->

## Reproduce

One script consumes one inert data spec:

```sh
./scripts/test262/test-test262.sh --spec dev-support/test262/current.conf --check
./scripts/test262/test-test262.sh --spec dev-support/test262/current.conf --runner-provenance
./scripts/test262/test-test262.sh --spec dev-support/test262/current.conf --focused
TEST262_WORKERS=2 ./scripts/test262/test-test262.sh \
  --spec dev-support/test262/current.conf --full
```

`--check` authenticates the in-tree upstream pin, profile, negative-diagnostic
contract and phase/type-only exemption ledger, focused manifest, and frozen
TSV/JSONL receipts. `--runner-provenance` builds the Rust runner with the
current source fingerprint, verifies the embedded binding, and executes no
Test262 cases. `--focused` replays the 6,844-variant focused vector and requires
the result body, everything after the identity line, to be byte-identical;
public CI runs it on every pull request, every push to main, and a
`test262-focused` dispatch. The focused vector covers dependency-closed
private-callable, static import-attributes, and static JSON module cases;
source-authenticated dynamic-import and top-level-await syntax, runtime, graph,
and rejection cohorts; and the dependency-free module local-binding family.
`--full` runs all 102,037 variants and compares the complete result body against
the frozen `full_*_body_sha256` hashes; the scheduled `test262-full` job and the
`test262-receipt` dispatch cover it.

## The dropped variants

The runner `run-test262` (`conformance/test262`) has one backend, the engine
through `api::core`'s Test262 host. The frozen dropped-variant list
[`dropped-variants.tsv`](../dev-support/test262/dropped-variants.tsv)
is part of the metric's definition: it names, with a reason each, the D =
1,897 variants that pass with the five features of the `TARGET-*` entries of
the [deviation ledger](deviations.md) and fail without them:

| Reason | Ledger entry | Variants |
| --- | --- | ---: |
| `species` | `TARGET-SPECIES-001` | 528 |
| `direct-eval` | `TARGET-DIRECT-EVAL-001` | 517 |
| `realm` | `TARGET-REALMS-001` | 488 |
| `with` | `TARGET-WITH-001` | 311 |
| `unscopables` | `TARGET-UNSCOPABLES-001` | 53 |

These variants fail by decision. 27 of them pass anyway (negative `with` tests
whose expected early error the front end gives either way); the other 1,870
are among the 1,913 failures above. Every other failing variant is one that
QuickJS 2026-06-04 also fails, as its `test262_errors.txt` lists; a failure
outside both is a defect. `current.conf` authenticates the list by its line
count, hash and `dropped_variants_count`.

## Promotion

Every spec key refers to in-tree data. When outcomes change, only frozen data
and hashes are updated.

- Focused outcomes: regenerate the frozen focused receipts, update
  `focused_tsv_sha256`/`focused_jsonl_sha256` and their line counts, and set
  `engine_semantics_sha256` to the receipt identity reported by
  `test262-engine-fingerprint.mjs --worktree`.
- Full outcomes: dispatch `test262-receipt`; the capture step prints
  `full_tsv_body_sha256` and `full_jsonl_body_sha256` for `current.conf`.
- Every gate then replays the frozen vectors and compares result bodies
  directly.

Negative admissions are fail-closed: an expected failure counts only when its
exact path is present in the audited-negative data. Every admitted path/variant
belongs to exactly one diagnostic class. Exact contracts require the
pinned-QuickJS phase, type, message, and `exact` or `absent` location policy. A
frozen ledger identifies the 2,706 variants that check phase and type only:
2,586 legacy ones, and 120 negative tests of a fork feature
(`fork-feature-phase-type-only`), whose tests pinned QuickJS skips, so it has
no diagnostic to match; the runner refuses that reason for a test of no feature
in the profile's `[fork-features]`.
Schema-v5 receipts authenticate both data files and record expected/actual
diagnostic fields. New negative admissions add exact contracts. The semantic
rule registry is separately authenticated, and scheduled QuickJS differential CI
replays every exact variant against QuickJS 2026-06-04 so the stored message and
location stay tied to the QuickJS oracle.

Archived milestone profiles, gates, and result vectors are release assets
inventoried in
[`dev-support/test262/archive/index.tsv`](../dev-support/test262/archive/index.tsv).
