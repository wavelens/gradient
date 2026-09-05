# Diagnostic reports

When an evaluation misbehaves, most of what explains it is in tables the UI
never shows: the dispatch-gate flags on `derivation_build`, the
`InputsUnavailable` attempt history behind a self-heal loop, `worker_connection`
disconnect reasons, upstream probe metrics, and the resolved server settings
that decide how any of it should be read.

A diagnostic report packages all of that into one SQLite file you can attach to
a bug report, so a maintainer can answer the question without access to your
instance.

## Generating one

On a task page, open the three-dot menu on the evaluation panel and choose
**Diagnostic report**. Four independent options control what goes in; every box
ticked is the fullest report you may generate. The download starts when you
press Generate, and building one takes a few minutes on a large evaluation.

The same thing over the API. The endpoint asks what to *anonymise*, which is the
inverse of what the dialog asks:

```sh
curl -sO -J -H "Authorization: Bearer $TOKEN" \
  "https://gradient.example/api/v1/evals/$EVAL_ID/report?anonymize_packages=true"
```

## What the options do

| Option | Default | API parameter | Effect |
| --- | --- | --- | --- |
| Include identities | off | `anonymize_identities=true` | Off by default: repository URLs, project and task names, user emails, worker names and ids become per-report tokens (`repo-a1b2`, `worker-7f3c`). |
| Include package names | on | `anonymize_packages=false` | On by default, because knowing *which* package broke is usually what makes a report useful. Unticked, the name half of store paths and flake attribute paths becomes a token. |
| Include build logs | off | `include_logs=false` | Off by default: the full log of every failed or aborted attempt, carrying whatever the build printed. Successful builds are never included. |
| Include instance context | on | `include_instance=true` | Worker fleet, upstream caches and the resolved server config. Requires the `ManageWorkers` permission. |

The API keeps its own defaults for callers that omit a parameter
(`anonymize_identities=true`, `anonymize_packages=false`, `include_logs=true`,
`include_instance=true`); the dialog always sends all four explicitly.

Anonymisation is stable pseudonymisation, not deletion. The same input always
maps to the same token within one report, so dependency and closure reasoning
still works; a fresh salt per report means two reports of the same instance
cannot be correlated. The salt is never written to the file.

Free text - build logs and commit messages - is rewritten against every
pseudonym the report has minted, in one pass over the text. Both the pass and
the pseudonym set it is compiled from are shared across every log, so the cost
of anonymising a report grows with its size rather than with its size times its
package count.

Nix store **hashes are always preserved**, even with everything anonymised.
They are one-way, and they are what lets a maintainer check whether a path is
available on a public cache, which is exactly the class of bug that motivates
most reports.

## What is never in the file

API keys, sessions, device-authorization records, worker registration token
hashes, upstream cache API keys, user password hashes and forge app credentials
are absent, not redacted. Every exported column is named explicitly in the
extractor, so a table that later gains a secret column cannot start exporting
it.

The `report_manifest` table records, for every table included, how many rows it
holds against how many existed, what its scope selected and which filter was
applied. A report generated without logs says so; it never looks like an
evaluation that had none.

Read the `scope` column before drawing conclusions from a count. Only the
evaluation's own tables are the evaluation's alone: builds hang off
`derivation_build` anchors that are shared with every other evaluation that
built the same derivation, so `build_attempt`, `phase_event` and the
`derivation*` tables carry rows made for other evaluations, and the file will
show attempts older than the evaluation itself. `dispatched_job_phase` is not
one of those: it hangs off this evaluation's own dispatched jobs.
`worker_registration` and
`upstream_metric` describe the whole instance; `worker_connection` and
`worker_sample` cover the workers that ran this evaluation, for as long as it
ran.

## Reading one

The file is an ordinary SQLite database, so any client opens it:

```sh
sqlite3 gradient-report-01a05a38-2026-09-01.db \
  "SELECT name, status FROM derivation_build JOIN derivation ON derivation.id = derivation_build.derivation"
```

`dispatched_job_phase` holds the worker's phase timeline, one row per span,
nested through `parent_seq`. `phase` is the numeric discriminant; the names are
listed in [the job board page](usage/job-board.md). To see where a job's time
actually went:

```sh
sqlite3 gradient-report-01a05a38-2026-09-01.db \
  'SELECT j.worker_id, p.phase, p.end_ms - p.start_ms AS ms
     FROM dispatched_job_phase p
     JOIN dispatched_job j ON j.id = p.dispatched_job
    ORDER BY ms DESC LIMIT 20'
```

`dispatched_job.outcome` says how each job ended (0 completed, 1 failed); it is
null for a job still running when the report was taken, and for one whose worker
disconnected without reporting.

`commit` is a reserved word in SQLite, so the revision table needs quoting:

```sh
sqlite3 gradient-report-01a05a38-2026-09-01.db \
  'SELECT hash, author_name, message FROM "commit"'
```

The inspector adds curated views over the same file. It is called
`gradient-report`, is on `PATH` inside `nix develop`, and runs standalone as
`nix run .#gradient-report`:

```
gradient-report REPORT summary      status, timings, build and failure counts
gradient-report REPORT timeline     phase events, dispatches and attempts in order
gradient-report REPORT why-stuck    which gate each waiting anchor is held by
gradient-report REPORT failed       failed attempts; --log ATTEMPT dumps one
gradient-report REPORT workers      registration and connection history
gradient-report REPORT manifest     what the report contains and what it left out
gradient-report REPORT sql "QUERY"  raw access
```

`why-stuck` is the one to reach for first on a hung evaluation: for every anchor
that never reached a terminal state it names which of `edges_complete`,
`closure_complete` and `drv_closure_cached` is false, and lists the dependencies
still unfinished underneath it.

The inspector refuses a report whose schema version it does not recognise rather
than answering from whichever columns still happen to line up. If that happens,
the report came from a newer Gradient than the tool.
