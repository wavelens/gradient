<!--
SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
SPDX-License-Identifier: AGPL-3.0-only
-->

# Artefacts

Build outputs can offer files for download, and a fixed link can point to the newest build of a task.

**Requirements:**

- A task with a [wildcard](../reference/wildcards.md) matching the package.
- An [API key](../reference/api.md#api-keys) for links into a private project.

## 1. Declare the Files

Builds list their downloadable files in `$out/nix-support/hydra-build-products`, in the Hydra format.

```nix
# package.nix
postInstall = ''
  mkdir -p $out/nix-support
  echo "file binary-dist $out/bin/gradient" >> $out/nix-support/hydra-build-products
'';
```

| Field | Example | Meaning |
|---|---|---|
| Type | `file`, `doc` | Kind of the file, shown as a badge |
| Subtype | `binary-dist`, `html` | Finer kind. HTML files open in a new browser tab |
| Path | `$out/bin/gradient` | File or directory inside the output |

- Lines name a file each, with the three fields separated by spaces.
- Directories download as a `<name>.tar.zst` archive.

## 2. Find the Artefacts

=== "UI"

    Open the task, then the menu of an entry point, then **Artefacts**. Entry points with files show a box icon. Menu entries for unfinished builds appear disabled.

    Rows on the **Build Artefacts** page show the type, name and size of each file, next to a **Download** or **Open** button.

=== "CLI"

    ```sh
    gradient download
    ```

    The [CLI](../reference/cli.md) can pick the evaluation and the files interactively.

## 3. Link the Newest Build

Fixed links read the newest evaluation of a task.

```text
https://gradient.example.com/api/v1/tasks/<project>/<task>/entry-point-downloads?eval=<attribute>&filename=<file>
```

| Parameter | Value |
|---|---|
| `<project>`, `<task>` | Names of the project and the task |
| `eval` | Attribute path of the entry point, such as `packages.x86_64-linux.gradient-cli-static`. URL-encode `"` as `%22` |
| `filename` | File name from `hydra-build-products`, such as `gradient` |
| `token` | Optional API key for a private project |

Real-world example from the Gradient CLI install.

```sh
curl -fLo gradient "https://public.gradient.ci/api/v1/tasks/gradient/main/entry-point-downloads?eval=packages.x86_64-linux.gradient-cli-static&filename=gradient"
```

- Links to an unchanged derivation keep working during a new evaluation.
- Links to a changed derivation return `404` until the new build is done (`Completed` or `Substituted`). Downloads from the previous build remain unavailable meanwhile.
- Markdown pages can embed the link as a button: `[Download](<link>){ .md-button }`.

## 4. Share Private Files

Public projects need no credentials. Private projects offer two kinds of links.

| Link | Lifetime | Use |
|---|---|---|
| `entry-point-downloads?...&token=GRAD...` | Until the API key expires | Fixed links for scripts and deployments |
| `/builds/<build>/download/<file>?token=<token>` | 1 hour | Sharing a file of a specific build |

- Create a dedicated API key for fixed links, pinned to the project, with `viewProject` only and `allowed_ips` where possible.
- Request the 1-hour token from `GET /builds/<build>/download-token`. Links on the **Build Artefacts** page include this token already.

!!! warning
    Tokens in a URL end up in shell histories and proxy logs. Prefer the 1-hour token for links shared with people.

## Verify Deployment

```sh
curl -fsSI "https://gradient.example.com/api/v1/tasks/<project>/<task>/entry-point-downloads?eval=<attribute>&filename=<file>"
```

Working links answer `200` with a `Content-Disposition` header naming the file.

## Next Steps

- [Evaluation Wildcards](../reference/wildcards.md)
- [CLI](../reference/cli.md)
- [API](../reference/api.md)
- [Settings](../ui/settings.md)
