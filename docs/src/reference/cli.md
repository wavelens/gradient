# CLI

Every command in `gradient`, taken from the CLI's `--help` output.

Commands target the selected project and task. To act on a different one, pass it as an argument, or use `--project` on `gradient build`.

## Install

=== "Linux binary"

    ```sh
    curl -fLo gradient "https://public.gradient.ci/api/v1/tasks/gradient/main/entry-point-downloads?eval=packages.x86_64-linux.gradient-cli-static&filename=gradient"
    chmod +x gradient && sudo mv gradient /usr/local/bin/
    ```

    [Download Gradient CLI](https://public.gradient.ci/api/v1/tasks/gradient/main/entry-point-downloads?eval=packages.x86_64-linux.gradient-cli-static&filename=gradient){ .md-button }

    A static x86_64 binary from the latest `main`, built by the public instance. Nix support is absent, so Nix is not required.

=== "NixOS"

    ```nix
    environment.systemPackages = [ inputs.gradient.packages.${pkgs.system}.gradient-cli ];
    ```

=== "Without installing"

    ```sh
    nix run github:wavelens/gradient/latest#gradient-cli -- --help
    ```

| Package | Contains |
|---|---|
| `gradient-cli` | Every command below except `eval` |
| `gradient-cli-full` | Also `gradient eval`, linked against libnix |
| `gradient-cli-static` | One static musl binary without Nix support. `gradient build` downloads products into `result/`, and `cache upload` accepts only `--nar-file` |

## Configuration

```sh
gradient login https://gradient.example.com
```

- `gradient login` opens a browser window for login confirmation.
- Pass `--no-browser` to get the login URL printed in the terminal instead.
- For scripts, use `--username` and `--password` to log in without a browser.
- With multiple projects available, the CLI asks which one to select.
- Settings live in `~/.config/gradient/config.toml` (or `$XDG_CONFIG_HOME/gradient`). This file stores the server address, token and selections.
- `gradient config <key> [value]` can read or write one of `server`, `authtoken`, `selectedproject`, `selectedtask`, `selectedbuild`.
- Add `--json` to any command for machine-readable output with all prompts disabled.
- A server behind a private CA works once the CA certificate is in the system trust store, e.g. `security.pki.certificateFiles` on NixOS.

## Account

| Command | Options | Description |
|---|---|---|
| `gradient config <KEY> [VALUE]` | - | Read or update a configuration value |
| `gradient status` | - | Show whether the server connection works |
| `gradient register` | `--username <USERNAME>`, `--name <NAME>`, `--email <EMAIL>`, `--password <PASSWORD>` | Create a new user account |
| `gradient login [SERVER]` | `--username <USERNAME>`, `--password <PASSWORD>`, `--no-browser` | Authenticate against a server |
| `gradient logout` | - | End the current session |
| `gradient info` | - | Print details of the logged-in user |
| `gradient hash` | - | Produce an argon2id PHC string from a password, for `services.gradient.state.users.<name>.password_file` |

## Projects and Tasks

| Command | Options | Description |
|---|---|---|
| `gradient project select <PROJECT>` | - | Choose the project that later commands target |
| `gradient project create` | `--name <NAME>`, `--display-name <DISPLAY_NAME>`, `--description <DESCRIPTION>` | Create a project. Fields left empty open in `$EDITOR` |
| `gradient project show` | - | Display the currently selected project |
| `gradient project list` | - | List all projects owned by the current user |
| `gradient project edit` | `--new-name <NEW_NAME>`, `--display-name <DISPLAY_NAME>`, `--description <DESCRIPTION>` | Change details of the selected project |
| `gradient project delete` | - | Remove the selected project |
| `gradient project user list` | - | Show all project members |
| `gradient project user add <USER> [ROLE]` | - | Grant a user access to the project |
| `gradient project user remove <USER>` | - | Revoke a user's access to the project |
| `gradient project ssh show` | - | Print the public SSH key used for repository access |
| `gradient project ssh recreate` | - | Generate a new SSH key pair and print the new public key |
| `gradient project cache list` | - | Show all caches the project subscribes to |
| `gradient project cache add <CACHE>` | - | Subscribe the project to a cache |
| `gradient project cache remove <CACHE>` | - | End the project's subscription to a cache |
| `gradient task select <TASK>` | - | Choose the task that later commands target, within the selected project |
| `gradient task show` | - | Display the selected task, its last evaluation and its builds |
| `gradient task log` | - | Stream logs from the last evaluation of the selected task |
| `gradient task create` | `--name <NAME>`, `--display-name <DISPLAY_NAME>`, `--description <DESCRIPTION>`, `--repository <REPOSITORY>`, `--wildcard <WILDCARD>` | Create a task in the selected project. Fields left empty open in `$EDITOR` |
| `gradient task list` | - | List all tasks in the selected project |
| `gradient task edit` | `--new-name <NEW_NAME>`, `--display-name <DISPLAY_NAME>`, `--description <DESCRIPTION>`, `--repository <REPOSITORY>`, `--wildcard <WILDCARD>` | Change details of the selected task |
| `gradient task delete` | - | Remove the selected task |
| `gradient task evaluate` | - | Trigger a new evaluation of the selected task |

## Builds and Evaluations

| Command | Options | Description |
|---|---|---|
| `gradient build [TARGET]` | `--system <SYSTEM>`, `--project <PROJECT>`, `--background`, `--quiet`, `--no-link`, `--override-input <INPUT> <FLAKE>` | Submit a build request from the current git repository |
| `gradient watch <EVALUATION>` | - | Watch a running evaluation's live build logs and status |
| `gradient logs <EVALUATION>` | - | Print the full logs of every build in an evaluation |
| `gradient download [FLAKE_REF]` | `--evaluation <EVALUATION>`, `--task <TASK>`, `--products <PRODUCTS>`, `--out <OUT>` | Download evaluation artefacts |
| `gradient builds graph <ID>` | `--interactive` | Show a build's dependency graph |
| `gradient builds log <ID>` | `--interactive`, `--lines <LINES>`, `--search <SEARCH>`, `--case` | View a build's log |
| `gradient eval <PATTERN>...` | `--workers <N>`, `--max-rss <MiB>`, `--max-memory <MiB>` | Evaluate a flake to derivations locally, a JSON line per attribute. Only in `gradient-cli-full`, see [below](#local-evaluation) |

## Caches

| Command | Options | Description |
|---|---|---|
| `gradient cache create` | `--name <NAME>`, `--display-name <DISPLAY_NAME>`, `--description <DESCRIPTION>`, `--priority <PRIORITY>`, `--max-storage-gb <MAX_STORAGE_GB>` | Create a cache. Fields left empty open in `$EDITOR` for completion |
| `gradient cache list` | - | List all caches owned by the current user |
| `gradient cache edit <NAME>` | `--display-name <DISPLAY_NAME>`, `--description <DESCRIPTION>`, `--priority <PRIORITY>`, `--max-storage-gb <MAX_STORAGE_GB>` | Change settings of an existing cache |
| `gradient cache delete <NAME>` | - | Remove a cache |
| `gradient cache show <NAME>` | - | Print the public key of a cache |
| `gradient cache install-netrc --server <SERVER> --cache <CACHE>` | `--server <SERVER>`, `--token <TOKEN>`, `--cache <CACHE>`, `--netrc-file <NETRC_FILE>` | Write a netrc entry for a cache from an existing API key. The server is not contacted. Run as root to write `/etc/nix/netrc` |
| `gradient cache nar list <CACHE>` | `--hash <HASH>`, `--package <PACKAGE>`, `--sort <SORT>`, `--order <ORDER>`, `--page <PAGE>`, `--per-page <PER_PAGE>`, `--interactive` | List the NARs stored in a cache |
| `gradient cache nar show <CACHE> <HASH>` | - | Print the full metadata of one NAR |
| `gradient cache nar delete <CACHE> <HASH>` | `--yes` | Remove a NAR from a cache |
| `gradient cache nar stats <CACHE>` | - | Print aggregate statistics over all NARs in a cache |
| `gradient cache upload <CACHE> [PATHS]...` | `--nar-file <NAR_FILE>`, `--narinfo <NARINFO>`, `--no-closure` | Upload one or more NARs to a cache |

## Workers

| Command | Options | Description |
|---|---|---|
| `gradient worker register --display-name <DISPLAY_NAME> <WORKER_ID>` | `--display-name <DISPLAY_NAME>`, `--url <URL>`, `--token <TOKEN>` | Add a new worker to the selected project |
| `gradient worker list` | - | Show every worker in the selected project |
| `gradient worker delete <WORKER_ID>` | - | Remove a worker from the selected project |

## Tools

| Command | Options | Description |
|---|---|---|
| `gradient generate apikey` | - | Create an API token plus the digest needed for a declarative `api_keys.<name>.key_file`, see [API Key Files](../concepts/declarative-state.md#api-key-files) |
| `gradient mcp` | `--control` | Expose this Gradient instance to MCP clients over stdio |

## Interactive Mode

Add `-i` to switch a command from plain output to a full-screen view. This flag conflicts with `--json`.

| Command | View | Keys |
|---|---|---|
| `gradient cache nar list -i` | NAR browser with filter | Type to filter, arrows to move, `Esc` to quit |
| `gradient builds graph <id> -i` | Dependency tree, like `nix-tree` | Arrows to move, `Enter` to expand, `Esc` to quit |
| `gradient builds log <id> -i` | Log pager | Arrows to scroll, `f` to follow, `/` to search, `Esc` to quit |

## Download Filters

An interactive choice of evaluation and build products in `gradient download`. `--evaluation` and `--products all` (or `1,3-5`) are skipping the pickers. A positional `'#packages.x86_64-linux.app'` (comma-separated for more) can select by attribute instead of `--products`.

## Local Evaluation

Run the worker's evaluator on the local machine with `gradient eval`, in the same way as `nix-eval-jobs`. Output is one JSON line per attribute with `attr`, `attrPath` and `drvPath`. A failed attribute produces a line with `error`.

```sh
gradient eval 'packages.x86_64-linux.*'             # the flake in the current directory
gradient eval .#hello                              # a concrete attribute, as fast as nix eval
gradient eval 'github:NixOS/patchelf#hydraJobs.*'  # any flake ref before the #
```

Local flakes in a Git checkout behave like `nix eval .`. Only tracked files reach the store.

| Flag | Default | Behavior |
|---|---|---|
| `--workers <N>` | core count, capped at 16 | Evaluation subprocesses in the pool |
| `--max-rss <MiB>` | `2048` | Ceiling on the resident memory of an idle subprocess between calls |
| `--max-memory <MiB>` | unset | Ceiling on the resident memory of the whole pool |

The pool holds one subprocess per `--workers`. Its eval cache lives in the user's cache directory, kept separate from `nix eval`.
