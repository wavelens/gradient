# CLI

Every `gradient` command, generated from the CLI's `--help`. Commands act on the selected project and task unless `--project` or an argument names another.

## Install

=== "NixOS"

    ```nix
    environment.systemPackages = [ inputs.gradient.packages.${pkgs.system}.gradient-cli ];
    ```

=== "Without installing"

    ```sh
    nix run github:wavelens/gradient#gradient-cli -- --help
    ```

| Package | Contains |
|---|---|
| `gradient-cli` | Every command below except `eval` |
| `gradient-cli-full` | Also `gradient eval`, which links libnix |

## Configuration

```sh
gradient login https://gradient.example.com
```

- Opens the browser to confirm the login; `--no-browser` prints the URL instead, `--username` and `--password` skip the browser for scripts.
- With several projects, the CLI asks which one to select.
- Server, token and selections live in `~/.config/gradient/config.toml` (`$XDG_CONFIG_HOME/gradient`).
- `gradient config <key> [value]` reads or sets one of `server`, `authtoken`, `selectedproject`, `selectedtask`, `selectedbuild`.
- `--json` on any command prints machine-readable output and disables prompts.
- A server behind a private CA works once the CA is in the system trust store, e.g. `security.pki.certificateFiles` on NixOS.

## Account

| Command | Options | Description |
|---|---|---|
| `gradient config <KEY> [VALUE]` | - | Get or set configuration values |
| `gradient status` | - | Check server connection status |
| `gradient register` | `--username <USERNAME>`, `--name <NAME>`, `--email <EMAIL>`, `--password <PASSWORD>` | Register a new user account |
| `gradient login [SERVER]` | `--username <USERNAME>`, `--password <PASSWORD>`, `--no-browser` | Login to the server |
| `gradient logout` | - | Logout from the server |
| `gradient info` | - | Display current user information |
| `gradient hash` | - | Hash a password as an argon2id PHC string for use in `services.gradient.state.users.<name>.password_file` |

## Projects and Tasks

| Command | Options | Description |
|---|---|---|
| `gradient project select <PROJECT>` | - | Select the default project |
| `gradient project create` | `--name <NAME>`, `--display-name <DISPLAY_NAME>`, `--description <DESCRIPTION>` | Create a project |
| `gradient project show` | - | Show the selected project |
| `gradient project list` | - | List projects |
| `gradient project edit` | `--new-name <NEW_NAME>`, `--display-name <DISPLAY_NAME>`, `--description <DESCRIPTION>` | Edit a project |
| `gradient project delete` | - | Delete a project |
| `gradient project user list` | - | List members |
| `gradient project user add <USER> [ROLE]` | - | Invite a member |
| `gradient project user remove <USER>` | - | Remove a member |
| `gradient project ssh show` | - | Show the project's public SSH key |
| `gradient project ssh recreate` | - | Generate a new SSH key; the old key stops working |
| `gradient project cache list` | - | List subscribed caches |
| `gradient project cache add <CACHE>` | - | Subscribe to a cache |
| `gradient project cache remove <CACHE>` | - | Unsubscribe from a cache |
| `gradient task select <TASK>` | - | Select the default task |
| `gradient task show` | - | Show the selected task and stream its newest evaluation |
| `gradient task log` | - | Stream the logs of the task's newest evaluation |
| `gradient task create` | `--name <NAME>`, `--display-name <DISPLAY_NAME>`, `--description <DESCRIPTION>`, `--repository <REPOSITORY>`, `--wildcard <WILDCARD>` | Create a task |
| `gradient task list` | - | List tasks |
| `gradient task edit` | `--new-name <NEW_NAME>`, `--display-name <DISPLAY_NAME>`, `--description <DESCRIPTION>`, `--repository <REPOSITORY>`, `--wildcard <WILDCARD>` | Edit a task |
| `gradient task delete` | - | Delete a task |
| `gradient task evaluate` | - | Start an evaluation |

## Builds and Evaluations

| Command | Options | Description |
|---|---|---|
| `gradient build [TARGET]` | `--system <SYSTEM>`, `--project <PROJECT>`, `--background`, `--quiet`, `--no-link`, `--override-input <INPUT> <FLAKE>` | Submit a build request from the current git repository |
| `gradient watch <EVALUATION>` | - | Watch a running evaluation's live build logs and status |
| `gradient logs <EVALUATION>` | - | Print the full logs of every build in an evaluation |
| `gradient download [FLAKE_REF]` | `--evaluation <EVALUATION>`, `--task <TASK>`, `--products <PRODUCTS>`, `--out <OUT>` | Download evaluation artefacts |
| `gradient builds graph <ID>` | `--interactive` | Show a build's dependency graph |
| `gradient eval <PATTERN>...` | - | Evaluate a flake to derivations locally, one JSON line per attribute; only in `gradient-cli-full`, see [below](#local-evaluation) |
| `gradient builds log <ID>` | `--interactive`, `--lines <LINES>`, `--search <SEARCH>`, `--case` | View a build's log |

## Caches

| Command | Options | Description |
|---|---|---|
| `gradient cache create` | `--name <NAME>`, `--display-name <DISPLAY_NAME>`, `--description <DESCRIPTION>`, `--priority <PRIORITY>`, `--max-storage-gb <MAX_STORAGE_GB>` | Create a cache |
| `gradient cache list` | - | List caches |
| `gradient cache edit <NAME>` | `--display-name <DISPLAY_NAME>`, `--description <DESCRIPTION>`, `--priority <PRIORITY>`, `--max-storage-gb <MAX_STORAGE_GB>` | Edit a cache |
| `gradient cache delete <NAME>` | - | Delete a cache |
| `gradient cache show <NAME>` | - | Show a cache |
| `gradient cache install-netrc --server <SERVER> --cache <CACHE>` | `--server <SERVER>`, `--token <TOKEN>`, `--cache <CACHE>`, `--netrc-file <NETRC_FILE>` | Build a netrc entry locally for a cache and install it into a netrc file. No request is sent to the server - bring your own API key (issued via the frontend or `gradient` UI). Intended to be run as root (e.g. sudo) to install into /etc/nix/netrc |
| `gradient cache nar list <CACHE>` | `--hash <HASH>`, `--package <PACKAGE>`, `--sort <SORT>`, `--order <ORDER>`, `--page <PAGE>`, `--per-page <PER_PAGE>`, `--interactive` | List NARs in a cache |
| `gradient cache nar show <CACHE> <HASH>` | - | Show a NAR's full metadata |
| `gradient cache nar delete <CACHE> <HASH>` | `--yes` | Delete a NAR from a cache |
| `gradient cache nar stats <CACHE>` | - | Aggregate stats for a cache's NARs |
| `gradient cache upload <CACHE> [PATHS]...` | `--nar-file <NAR_FILE>`, `--narinfo <NARINFO>`, `--no-closure` | Upload NAR(s) to a cache |

## Workers

| Command | Options | Description |
|---|---|---|
| `gradient worker register --display-name <DISPLAY_NAME> <WORKER_ID>` | `--display-name <DISPLAY_NAME>`, `--url <URL>`, `--token <TOKEN>` | Register a new worker under the selected project |
| `gradient worker list` | - | List all workers registered under the selected project |
| `gradient worker delete <WORKER_ID>` | - | Unregister a worker from the selected project |

## Tools

| Command | Options | Description |
|---|---|---|
| `gradient generate apikey` | - | Generate an API token and its `GRAD` client form; `key_file` needs the digest instead, see [API Key Files](../concepts/declarative-state.md#api-key-files) |
| `gradient mcp` | `--control` | Serve this Gradient instance to MCP clients over stdio |

## Interactive Mode

`-i` opens a full-screen view instead of plain output; `--json` ignores the flag.

| Command | View | Keys |
|---|---|---|
| `gradient cache nar list -i` | NAR browser with filter | Type to filter, arrows move, `Esc` quits |
| `gradient builds graph <id> -i` | Dependency tree, like `nix-tree` | Arrows move, `Enter` expands, `Esc` quits |
| `gradient builds log <id> -i` | Log pager | Arrows scroll, `f` follows, `/` searches, `Esc` quits |

## Download Filters

`gradient download` picks an evaluation and its build products interactively. `--evaluation` and `--products all` (or `1,3-5`) skip the pickers; a positional `'#packages.x86_64-linux.app'` (comma-separated for several) selects by attribute instead of `--products`.

## Local Evaluation

`gradient eval` runs the worker's evaluator locally, like `nix-eval-jobs`: one JSON line per attribute with `attr`, `attrPath` and `drvPath`, or `error` for a failed attribute.

```sh
gradient eval 'packages.x86_64-linux.#'             # the flake in the current directory
gradient eval .#hello                              # one attribute, as fast as nix eval
gradient eval 'github:NixOS/patchelf#hydraJobs.*'  # any flake ref before the #
```

A local flake inside a Git checkout is evaluated like `nix eval .`: only tracked files reach the store.
