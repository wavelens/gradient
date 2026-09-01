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
**Diagnostic report**. Four independent options control what goes in; the
download starts when you press Generate.

The same thing over the API:

```sh
curl -sO -J -H "Authorization: Bearer $TOKEN" \
  "https://gradient.example/api/v1/evals/$EVAL_ID/report?anonymize_packages=true"
```

## What the options do

| Option | Default | Effect |
| --- | --- | --- |
| Anonymise identities | on | Repository URLs, project and task names, user emails, worker names and ids become per-report tokens (`repo-a1b2`, `worker-7f3c`). |
| Anonymise package names | off | The name half of store paths and flake attribute paths becomes a token. Off by default, because knowing *which* package broke is usually what makes a report useful. |
| Include build logs | on | The full log of every failed or aborted attempt. Successful builds are not included. |
| Include instance context | on | Worker fleet, upstream caches and the resolved server config. Requires the `ManageWorkers` permission. |

Anonymisation is stable pseudonymisation, not deletion. The same input always
maps to the same token within one report, so dependency and closure reasoning
still works; a fresh salt per report means two reports of the same instance
cannot be correlated. The salt is never written to the file.

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
holds against how many existed and which filter was applied. A report generated
without logs says so; it never looks like an evaluation that had none.

## Reading one

The file is an ordinary SQLite database, so any client opens it:

```sh
sqlite3 gradient-report-01a05a38-2026-09-01.db \
  "SELECT name, status FROM derivation_build JOIN derivation ON derivation.id = derivation_build.derivation"
```

The `gradient-report` inspector, on `PATH` inside `nix develop` and available as
`nix run .#report-inspector`, adds curated views over the same file:

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
