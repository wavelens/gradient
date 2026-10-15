# Artefacts

Every build can expose files for download. A stable link can always point to the most recent build of a task.

**Requirements:**

- A task whose [wildcard](../reference/wildcards.md) matches the package.
- An [API key](../reference/api.md#api-keys) for links to a private project.

## 1. Declare the Files

A build can declare downloadable files through `$out/nix-support/hydra-build-products`. Hydra picks up this file and shows the entries on the build page. Create the directory and append one line per file in a `postInstall` hook:

```nix
# package.nix
postInstall = ''
  mkdir -p $out/nix-support
  echo "file binary-dist $out/bin/gradient" >> $out/nix-support/hydra-build-products
'';
```

Each line contains three space-separated fields:

| Field | Example | Meaning |
|---|---|---|
| Type | `file`, `doc` | Kind of the file, shown as a badge |
| Subtype | `binary-dist`, `html` | Finer kind. HTML files open in a new browser tab |
| Path | `$out/bin/gradient` | File or directory inside the output |

- Use one line per file.
- Declare a directory to offer it as a `<name>.tar.zst` archive.

## 2. Find the Artefacts

=== "UI"

    Open the task. Open the menu of an entry point and choose **Artefacts**. Entry points that contain files show a box icon. While a build is still in progress, the menu entry appear disabled.

    On the **Build Artefacts** page, each row show the type, name and size of a file. Use **Download** or **Open** to retrieve or view it.

=== "CLI"

    ```sh
    gradient download
    ```

    The [CLI](../reference/cli.md) ask for the evaluation and the files interactively.

## 3. Link the Newest Build

A fixed link always points to the latest evaluation of a task.

```text
https://gradient.example.com/api/v1/tasks/<project>/<task>/entry-point-downloads?eval=<attribute>&filename=<file>
```

| Parameter | Value |
|---|---|
| `<project>`, `<task>` | Project name and task name |
| `eval` | Attribute path of the entry point, for example `packages.x86_64-linux.gradient-cli-static`. Encode `"` as `%22` |
| `filename` | File name taken from `hydra-build-products`, for example `gradient` |
| `token` | API key, needed only for private projects |

This command downloads the Gradient CLI:

```sh
curl -fLo gradient "https://public.gradient.ci/api/v1/tasks/gradient/main/entry-point-downloads?eval=packages.x86_64-linux.gradient-cli-static&filename=gradient"
```

- The link survives new evaluations as long as the derivation stay unchanged.
- After a derivation change, the link return `404` until the new build reach `Completed` or `Substituted`. The old build stay unavailable in the meantime.
- Markdown pages can render the link as a button: `[Download](<link>){ .md-button }`.

## 4. Share Private Files

Public projects serve files without credentials. Private projects support two link types.

| Link | Lifetime | Use |
|---|---|---|
| `entry-point-downloads?...&token=GRAD...` | Until the API key expires | Fixed links for scripts and deployments |
| `/builds/<build>/download/<file>?token=<token>` | 1 hour | Sharing a file of a specific build |

- For fixed links, create a dedicated API key. Pin it to the project, grant only `viewProject`, and restrict it with `allowed_ips` where the setup permits.
- For short-lived links, request a 1-hour token from `GET /builds/<build>/download-token`. Links on the **Build Artefacts** page already include this token.

!!! warning
    A token inside a URL ends up in shell histories and proxy logs. Use the 1-hour token for links shared with people.

## Verify Deployment

```sh
curl -fsSI "https://gradient.example.com/api/v1/tasks/<project>/<task>/entry-point-downloads?eval=<attribute>&filename=<file>"
```

A working link return `200` and include a `Content-Disposition` header with the file name.

## Next Steps

- [Evaluation Wildcards](../reference/wildcards.md)
- [CLI](../reference/cli.md)
- [API](../reference/api.md)
- [Settings](../ui/settings.md)
