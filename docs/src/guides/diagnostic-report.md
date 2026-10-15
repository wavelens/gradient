# Report a Bug with a Diagnostic Report

A diagnostic report is a single SQLite file that describes a stuck or failed evaluation. Attach it to a [bug report](https://github.com/wavelens/gradient/issues). Maintainers can diagnose the problem from the file alone and need no access to the instance.

**Requirements:**

- The affected evaluation, opened on its task page

## 1. Generate the Report

=== "UI"

    Open the task page and pick an evaluation. In the evaluation panel, use the three-dot menu and choose **Diagnostic report**, then **Generate**.

    Large evaluations can take several minutes to build.

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

- The options work as query parameters in the API: `anonymize_identities`, `anonymize_packages`, `include_logs`, `include_instance`.
- In the API, `include_logs` defaults to on.
- Within one report, one name always produces the same token.
- Dependencies remain readable.
- No one can link two reports to each other.
- Store hashes remain in the file, so a reader can check a path against public caches.

!!! note "Never in the File"
    API keys, sessions, passwords, worker tokens, upstream cache keys and Git host credentials stay out of the file entirely. The tool does not redact them; it omits them.

## 3. Attach the Report

GitHub rejects `.db` files as attachments, so compress the report before uploading:

```sh
gzip gradient-report-*.db
```

Then open a [Bug report (backend)](https://github.com/wavelens/gradient/issues/new?template=bug-report-backend.yml), describe the problem, and attach the resulting `gradient-report-<id>-<date>.db.gz` file.

## Verify Deployment

The browser will download `gradient-report-<id>-<date>.db`. The inspector will print the summary maintainers start from.

```sh
nix run github:wavelens/gradient/latest#gradient-report -- gradient-report-*.db summary
```

The inspector can read only the report schema of its own source revision. Reports from an older server need that release's tag, e.g. `github:wavelens/gradient/v1.4.0#gradient-report`.

## Next Steps

- [Diagnostic reports in depth](../contributors/diagnostic-reports.md) covers report tables, scopes and the `gradient-report` inspector.
- [Evaluations and Builds](../concepts/evaluations-and-builds.md) explains the statuses shown in the summary.
