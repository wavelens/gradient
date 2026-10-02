# Diagnostic Reports

A diagnostic report is one SQLite file with tables never shown in the UI. These are assignment gates on `derivation_build`, the attempt history behind a self-heal loop, disconnect reasons, upstream probe metrics and the resolved server settings. [Report a Bug](../guides/diagnostic-report.md) is covering generation and attachment. This page is covering the file's contents and how to read the data.

```mermaid
flowchart LR
    eval[Evaluation] --> export[Extractor]
    export --> db[(report .db)]
    db --> sqlite[sqlite3]
    db --> insp[gradient-report]
```

## Code

| Part | Path | Role |
|---|---|---|
| Extractor | `backend/gradient-report` | Writing the SQLite file: `schema.rs` (tables, `SCHEMA_VERSION`), `extract.rs`, `redact.rs`, `logs.rs`, `config_snapshot.rs` |
| Inspector | `nix/tools/report-inspector` | The `gradient-report` tool: Python stdlib only, running on any maintainer machine. Pytest suite in `tests/`, executed by the package build |

## Anonymisation

- Stable pseudonyms, not deletion. The same input is mapping to the same token within one report (`repo-a1b2`, `worker-7f3c`). Dependency reasoning is still working.
- A fresh salt per report. Two reports of one instance cannot be correlated. The salt is never written.
- Free text (build logs, commit messages) is rewritten against every pseudonym in one pass, shared across all logs.
- The rewrite cost is growing with the report size, not size times package count.
- Nix store **hashes always stay**. They are one-way and let a maintainer check a path against a public cache.

| API parameter | Default without the parameter |
|---|---|
| `anonymize_identities` | `true` |
| `anonymize_packages` | `false` |
| `include_logs` | `true` |
| `include_instance` | `true`, requiring `manageWorkers` |

The dialog is always sending all four explicitly.

## Never Exported

- API keys, sessions, device-authorization records, worker token hashes, upstream cache keys, password hashes and Git host credentials are absent, not redacted.
- Every exported column is named in the extractor. A table gaining a secret column later cannot start exporting that column.
- `cached_path_signature` is exporting only the presence of a signature, never the signature.

## Scope

`report_manifest` is recording per table the rows included against the rows existing, the scope and the filter. A report without logs is saying so and is never looking like an evaluation without logs.

| Scope | Tables |
|---|---|
| This evaluation only | The evaluation's own rows, `dispatched_job`, `dispatched_job_phase` |
| Shared builds | `build_attempt`, `phase_event`, `derivation*`: rows made for other evaluations of the same derivation, older attempts included |
| Whole instance | `worker_registration`, `base_worker`, `upstream_metric` |
| This project | `project_base_worker` |
| Workers of this evaluation | `worker_connection`, `worker_sample`, from creation until finish or the report |

- The `scope` column is worth reading before trusting a count.
- `worker_connection` and `worker_sample` carry no project. Their telemetry is describing the worker.
- `worker_registration` is empty under a base-worker fleet. The names are in `base_worker`.

## Closure Boundary

The start counters are counting edges. The far end of every edge is in the file. An absent row is then showing that the *instance* never had the path, not a skipped export.

| Table | Contents |
|---|---|
| `derivation`, `derivation_build`, `derivation_output` | The evaluation's derivations **and their direct dependencies** |
| `derivation_dependency` | The evaluation's own edges with `kind`, both ends exported |
| `cached_path` | Those derivations' outputs **and every path they reference** |
| `build_job` | The evaluation's jobs **and every job an exported attempt was running under** |

- One hop is enough. `blocking_deps` is counting one per build edge and reading the dependency's own shared build, outputs and cached paths.
- Deeper levels are summarised in the dependency's stored `blocking_deps`.
- `missing_runtime_deps` is the same count over the runtime dependencies (`derivation_dependency.kind IN (1, 2)`).
- A dependency row without a `build_job` row is evidence, not work of its own. `why-stuck` is telling the two apart this way.
- `build_job` is reaching past the evaluation. The substitute-miss budget is scoped per `(shared build, evaluation)` through `build_attempt.build_job`.

## Queries

Any SQLite client can open the file.

**Substitute-miss loops** per shared build and evaluation.

```sh
sqlite3 report.db \
  'SELECT d.name, j.evaluation, count(*) AS misses
     FROM build_attempt a
     JOIN build_job j ON j.id = a.build_job
     JOIN derivation_build db ON db.id = a.derivation_build
     JOIN derivation d ON d.id = db.derivation
    WHERE a.reason = 0
    GROUP BY 1, 2 HAVING misses > 2 ORDER BY misses DESC'
```

**Job Time per Phase:** `dispatched_job_phase` is holding one row per span, nested through `parent_seq`. `phase` is the numeric discriminant, named on the [Job Board](../ui/job-board.md#job-inspection).

```sh
sqlite3 report.db \
  'SELECT j.worker_id, p.phase, p.end_ms - p.start_ms AS ms
     FROM dispatched_job_phase p
     JOIN dispatched_job j ON j.id = p.dispatched_job
    ORDER BY ms DESC LIMIT 20'
```

`dispatched_job.outcome`: `0` completed, `1` failed, `2` abandoned (disconnect, restart, overdue abort), null while running.

**Cached but not served:** the cache is serving a path only with a `cached_path_signature` row for that cache and `signed = 1`. The query below is joining both before `derivation_output.is_cached` is trustworthy.

```sh
sqlite3 report.db \
  'SELECT cp.package, s.cache_name, s.signed
     FROM cached_path cp
     LEFT JOIN cached_path_signature s ON s.cached_path = cp.id
    WHERE s.id IS NULL OR s.signed = 0'
```

**Incomplete Closures:** a shared build has a *complete closure* when every output has a NAR and `missing_runtime_deps` is zero. `fetchable` and every assignment gate are reading this state. A non-zero count is keeping every assignment from trusting the shared build. A negative count is pointing at a lost ripple.

```sh
sqlite3 report.db \
  'SELECT d.name, b.missing_runtime_deps
     FROM derivation_build b JOIN derivation d ON d.id = b.derivation
    WHERE b.missing_runtime_deps <> 0
    ORDER BY b.missing_runtime_deps DESC'
```

- `cached_path.references` is the narinfo `References:` line behind the count, for checking a counter by hand.
- The server is recounting table-wide on every consistency check and logging mismatches as `runtime_drift`.
- `commit` is a reserved word and is only queryable in quotes as `"commit"`.

## Signs in the Data

| Shape | Meaning |
|---|---|
| Evaluation in `EvaluatingFlake` / `EvaluatingDerivation`, newest eval job has `finished_at` | The terminal report never landed. The `eval-completion-watchdog` pass is re-driving the transition |
| `evaluation_input_update` row on an active evaluation | No further input-update round is starting for the task while the row is active. A wedged one is stopping the flake updater |
| Shared build `Created`, `cache_available`, not wanted | Nothing will fetch the shared build. A cached output referencing the path is staying without a complete closure |

## Inspector

`gradient-report` is on `PATH` in `nix develop`. `nix run .#gradient-report` is enough outside the shell.

| Command | Output |
|---|---|
| `summary` | Status, timings, build and failure counts (default) |
| `timeline` | Phase events, assignments and attempts in order |
| `why-stuck` | The gate holding each waiting shared build |
| `failed` | Failed attempts. `--log ATTEMPT` is dumping one log |
| `workers` | Registration and connection history |
| `manifest` | The report's contents and omissions |
| `sql "QUERY"` | Raw access |
| `store-spec -o FILE` | A `gradient-daemon` store spec replaying the evaluation |

**`why-stuck`** is the first stop on a hung evaluation. The command is covering every shared build driven by the evaluation that never finished.

- The command is naming the gate (`walked`, `wanted`, `probed`, `blocking_deps`).
- The command is listing every dependency below that is not `fetchable`.
- The command is flagging a complete `.drv` closure as the only gate left when the report is missing that closure.

```text
vendor-registry: Queued, waiting on walked, blocking_deps = 1
    dep openssl-3.6.3 is a stub: never walked
    dep curl-8.21.0 walked over 2 unwalked inputs
```

| Dependency line | Meaning |
|---|---|
| `not in this report` | The file is lacking the row. A closed export has none, reports before schema 12 many |
| `is a stub: never walked` | A walk named the derivation but never read the derivation |
| `walked over N unwalked inputs` | `derivation.unwalked_inputs`: a subtree below was never recorded, as after a walk abandoned between batches |

- `wanted` is sitting outside the `(cache_available OR blocking_deps = 0)` arm and is stopping at passthroughs and finished builds.
- `ed-1.22.5: Created, waiting on wanted` is a shared build nothing will ever fetch.
- A stub or unwalked line is pointing at the walk, not the build.

## Schema Versions

- The inspector is reading exactly one schema (currently 18) and refusing every other. The message is naming both schemas.
- A schema bump is changing `SCHEMA_VERSION` in `backend/gradient-report/src/schema.rs` and `SUPPORTED_SCHEMA` in `nix/tools/report-inspector/gradient_report/db.py` together.
- The inspector package is failing to evaluate while the two differ.
- The schema is moving on its own, not with the release (12 to 16 inside 1.3.0).
- The matching inspector is built from the revision that wrote the report, with `nix build .#gradient-report`.
- A `nix develop` shell entered before a schema bump is keeping the old inspector until re-entered.
- Reports before schema 17 carry no base-worker telemetry.
