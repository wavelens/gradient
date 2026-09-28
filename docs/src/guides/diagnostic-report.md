# Report a Bug with a Diagnostic Report

One SQLite file with everything that explains a stuck or failed evaluation, ready to attach to a [bug report](https://github.com/wavelens/gradient/issues). Maintainers answer from the file, without access to the instance.

**Requirements:**

- The evaluation in question, on its task page

## 1. Generate the Report

=== "UI"

    On the task page, select the evaluation, then the three-dot menu on the evaluation panel -> **Diagnostic report** -> **Generate**. Building the file takes a few minutes on a large evaluation.

=== "API"

    ```sh
    curl -sOJ -H "Authorization: Bearer $TOKEN" \
      "https://gradient.example.com/api/v1/evals/$EVAL_ID/report?anonymize_identities=true&include_logs=false"
    ```

## 2. Choose What Goes In

| Option | Default | Effect |
|---|---|---|
| Include identities | off | Off: repository URLs, project, task, user and worker names become tokens such as `repo-a1b2` |
| Include package names | on | Off: package names in store paths and attributes become tokens |
| Include build logs | off | On: the full log of every failed or aborted attempt |
| Include instance context | on | Workers, upstream caches and the server settings; needs the `manageWorkers` permission |

- The same name always maps to the same token within one report, so dependencies stay readable; two reports cannot be linked.
- Store hashes stay, so maintainers can check a path against public caches.

!!! note "Never in the file"
    API keys, sessions, passwords, worker tokens, upstream cache keys and forge credentials are left out entirely, not redacted.

## 3. Attach the Report

Open an issue at <https://github.com/wavelens/gradient/issues> with what went wrong and attach `gradient-report-<id>-<date>.db`.

## Verify Deployment

The browser downloads `gradient-report-<id>-<date>.db`. The inspector prints the summary maintainers start from:

```sh
nix run github:wavelens/gradient/v1.4.0#gradient-report -- gradient-report-*.db summary
```

The inspector reads only the report schema of its own source revision, so the tag matches the server's version.

## Next Steps

- [Diagnostic reports in depth](../diagnostic-reports.md): tables, scopes and the `gradient-report` inspector
- [Evaluations and Builds](../concepts/evaluations-and-builds.md): statuses in the summary
