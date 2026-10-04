# Report a Bug with a Diagnostic Report

One SQLite file explaining a stuck or failed evaluation, for attaching to a [bug report](https://github.com/wavelens/gradient/issues). Maintainers answer from the file, without access to the instance.

**Requirements:**

- The evaluation in question, on its task page

## 1. Generate the Report

=== "UI"

    Select the evaluation on the task page. Then open the three-dot menu on the evaluation panel -> **Diagnostic report** -> **Generate**.

    Building the file can take a few minutes on a large evaluation.

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
| Include instance context | on | Workers, upstream caches and the server settings. The `manageWorkers` permission is required |

- The API can take the options as query parameters (`anonymize_identities`, `anonymize_packages`, `include_logs`, `include_instance`).
- `include_logs` is on by default in the API.
- The same name will always map to the same token within one report.
- Dependencies stay readable.
- Two reports cannot be linked.
- Store hashes stay in the file for checking a path against public caches.

!!! note "Never in the File"
    API keys, sessions, passwords, worker tokens, upstream cache keys and Git host credentials are left out entirely, not redacted.

## 3. Attach the Report

Open an issue at <https://github.com/wavelens/gradient/issues> with what went wrong and attach `gradient-report-<id>-<date>.db`.

## Verify Deployment

The browser will download `gradient-report-<id>-<date>.db`. The inspector will print the summary maintainers start from.

```sh
nix run github:wavelens/gradient/latest#gradient-report -- gradient-report-*.db summary
```

The inspector can read only the report schema of its own source revision. Reports from an older server need that release's tag, e.g. `github:wavelens/gradient/v1.4.0#gradient-report`.

## Next Steps

- [Diagnostic reports in depth](../contributors/diagnostic-reports.md): tables, scopes and the `gradient-report` inspector
- [Evaluations and Builds](../concepts/evaluations-and-builds.md): statuses in the summary
