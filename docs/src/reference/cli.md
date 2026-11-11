<!--
SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
SPDX-License-Identifier: AGPL-3.0-only
-->

# CLI

Every command in `gradient`, taken from the CLI's `--help` output.

Commands target the selected project and task. To act on a different one for a single command, pass the global `--project <NAME>` or `--task <NAME|PROJECT/NAME>` flag. The selection stays unchanged.

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
- Settings live in `~/.config/gradient/accounts.toml` (or `$XDG_CONFIG_HOME/gradient`). Only the owner can read the file, since it holds the login tokens.
- `gradient config <key> [value]` reads or writes one of `server`, `authtoken`, `selectedproject`, `selectedtask` for the current account.
- `GRADIENT_SERVER` and `GRADIENT_TOKEN` replace the saved server and token. Use them in a CI job with no config file, for example. A saved token can go only to the server that stored it. Pass `GRADIENT_TOKEN` too with another `GRADIENT_SERVER`. `gradient config server <url>` with another server can also clear the saved token.
- Tokens in the output of `gradient config authtoken` show only the first and last characters (example: `GRAD…1234`).
- Add `--json` to any command for machine-readable output with all prompts disabled.
- Without a terminal on stdin, or with `--json`, commands never open `$EDITOR`: pass every required field as a flag, otherwise the command fails and names the missing flag.
- Delete commands ask for confirmation in the terminal. Pass `--yes` (`-y`) to skip the prompt. The delete command fails without `--yes` when there's no terminal or `--json` is present.
- The `edit` commands open the current values in `$EDITOR` without a field flag. Use any flag to change only those fields and keep the rest untouched. The description can stay empty.
- A server behind a private CA works once the CA certificate is in the system trust store, e.g. `security.pki.certificateFiles` on NixOS.

## Multiple Accounts

Each account is a server with its own login, selected project and selected task. `gradient login` saves to the current account and makes that account the default.

```sh
gradient login https://gradient.example.com
gradient --account work login https://ci.work.example
gradient account list
gradient account switch work
```

- Pass `--account <NAME>` or set `GRADIENT_ACCOUNT` to run one command with another account.
- A `config.toml` from an older CLI version becomes the account `default`. The CLI only reads it and never changes it, so an older CLI version keeps working with it.
- If saved accounts exist but none is the default, for example after `gradient account remove` deleted the default one, commands fail until you run `gradient account switch <NAME>` or pass `--account <NAME>`.

| Command | Options | Description |
|---|---|---|
| `gradient account list` | - | List saved accounts, the default one marked with `*` |
| `gradient account switch [NAME]` | - | Switch the account used by later commands. Without a name, pick one from the list |
| `gradient account remove <NAME>` | - | Delete a saved account and its login token |

## Account

| Command | Options | Description |
|---|---|---|
| `gradient config <KEY> [VALUE]` | - | Read or update a configuration value |
| `gradient status` | - | Show whether the server connection works |
| `gradient register` | `--username <USERNAME>`, `--name <NAME>`, `--email <EMAIL>`, `--password <PASSWORD>` | Create a new user account |
| `gradient login [SERVER]` | `--username <USERNAME>`, `--password <PASSWORD>`, `--no-browser` | Authenticate against a server |
| `gradient logout` | - | Revoke the login token on the server, then delete the token from this machine. Revocation failures still delete the token locally. |
| `gradient info` | - | Print details of the logged-in user |
| `gradient hash` | - | Produce an argon2id PHC string from a password, for `services.gradient.state.users.<name>.password_file` |

## Logged-In User

| Command | Options | Description |
|---|---|---|
| `gradient logged-in-user ssh-public-keys list` | - | List SSH keys for the Nix daemon |
| `gradient logged-in-user ssh-public-keys add <NAME> [PUBLIC_KEY]` | `--file <FILE>` | Add an SSH public key. Provide the key inline or use `--file`. |
| `gradient logged-in-user ssh-public-keys delete <ID>` | - | Delete SSH key |
| `gradient logged-in-user api-keys list` | - | Show a list of API keys. |
| `gradient logged-in-user api-keys create <NAME>` | `--permission <PERMISSION>`, `--expires-in-days <DAYS>`, `--api-key-project <PROJECT>`, `--cache <CACHE>`, `--allowed-ip <IP>` | Create an API key and output it once. |
| `gradient logged-in-user api-keys edit <KEY>` | `--name <NAME>`, `--permission <PERMISSION>`, `--api-key-project <PROJECT>`, `--allowed-ip <IP>` | Change an API key. The specified permissions or IP addresses replace the current ones. |
| `gradient logged-in-user api-keys revoke <KEY>` | - | Revoke an API key. The API key can remain in the list for the audit trail. |
| `gradient logged-in-user api-keys delete <KEY>` | - | Delete API Key |
| `gradient logged-in-user api-keys permissions` | - | List the permissions that an API key can grant. |
| `gradient logged-in-user active-login-sessions list` | `--page <PAGE>`, `--per-page <PER_PAGE>` | List active login sessions |
| `gradient logged-in-user active-login-sessions revoke <ID>` | - | Revoke the session and sign out of that device. |
| `gradient logged-in-user invitations list` | - | List pending invitations to projects, caches and teams. Subscription requests and team grant requests appear in the `GET /user/requests` JSON output and on the **Requests** page. |
| `gradient logged-in-user invitations accept <TOKEN>` | - | Accept the invitation |
| `gradient logged-in-user invitations decline <TOKEN>` | - | Decline Invitation |
| `gradient logged-in-user linked-external-login-accounts list` | - | List login providers and show whether an account links to them. |
| `gradient logged-in-user linked-external-login-accounts disconnect <PROVIDER>` | - | Unlink an Account from a Login Provider |
| `gradient logged-in-user print-account-settings` | - | Print Settings for an Account |
| `gradient logged-in-user delete-account` | `--password <PASSWORD>`, `--confirm-username <USERNAME>` | Delete the account completely, then log out. Pass either `--password` or `--confirm-username` with `--json`. Prompts never appear in that mode. |

## Projects and Tasks

| Command | Options | Description |
|---|---|---|
| `gradient project select <PROJECT>` | - | Choose the project that later commands target |
| `gradient project create` | `--name <NAME>`, `--display-name <DISPLAY_NAME>`, `--description <DESCRIPTION>` | Create a project. Fields left empty open in `$EDITOR` |
| `gradient project show` | - | Display the currently selected project |
| `gradient project list` | - | List all projects owned by the current user |
| `gradient project edit` | `--new-name <NEW_NAME>`, `--display-name <DISPLAY_NAME>`, `--description <DESCRIPTION>` | Change details of the selected project |
| `gradient project delete` | `--yes` | Remove the selected project |
| `gradient project user list` | - | Show all project members |
| `gradient project user add <USER> [ROLE]` | - | Grant a user access to the project |
| `gradient project user remove <USER>` | - | Revoke a user's access to the project |
| `gradient project ssh show` | - | Print the public SSH key used for repository access |
| `gradient project ssh recreate` | - | Generate a new SSH key pair and print the new public key |
| `gradient project cache list` | - | Show all caches the project subscribes to |
| `gradient project cache add <CACHE>` | `--mode <MODE>` | Subscribe the project to a cache. Use `--mode` to choose ReadWrite, ReadOnly or WriteOnly. |
| `gradient project cache remove <CACHE>` | - | End the project's subscription to a cache |
| `gradient project secret list` | - | List secret names for the selected project. No values appear. |
| `gradient project secret show <NAME>` | - | Show who last set a secret and when. |
| `gradient project secret set <NAME>` | `--value <VALUE>` | Create or replace a secret. The command can read from piped stdin or a hidden prompt when `--value` is absent. |
| `gradient project secret delete <NAME>` | - | Delete secret |
| `gradient project show-sops-recipient` | - | Print the SOPS recipient to encrypt the repository's sops files for |
| `gradient project make-public` | - | Allow users to view selected projects without login. |
| `gradient project make-project-private` | - | Restrict selected projects to their members |
| `gradient project team-grant list` | - | List the teams that have access to a project, including pending requests. |
| `gradient project team-grant add <TEAM>` | `--role <ROLE>`, `--users`, `--workers` | Grant access to the team. A user without admin permissions for the team can send a request to a team admin. |
| `gradient project team-grant edit <TEAM>` | `--role <ROLE>`, `--users <BOOL>`, `--workers <BOOL>` | Change roles or access for team grants |
| `gradient project team-grant remove <TEAM>` | - | Delete the team grant or withdraw the pending request. |
| `gradient project pending-membership-invitation list` | - | List pending membership invitations for a project. |
| `gradient project pending-membership-invitation revoke <USER>` | - | Cancel Pending Invitations for a User |
| `gradient project webhook <VERB>` | see [Webhooks](#webhooks) | Manage webhooks for the selected project. See [Webhooks](#webhooks). |
| `gradient project git-host-connection list` | `--owner <OWNER>` | List the owner's connections to Git hosts and the connections that the owner can use. |
| `gradient project git-host-connection create` | `-n, --name <NAME>`, `--host-type <gitea\|forgejo\|gitlab\|github>`, `-d, --display-name <DISPLAY_NAME>`, `--base-url <URL>`, `--access-token <TOKEN>`, `--secret <SECRET>`, `--allowed-ips <IPS>`, `--receives-events <BOOL>`, `--reports-status <BOOL>` | Create a Git host connection. The Webhook URL and generated secret appear a single time. |
| `gradient project git-host-connection show <CONNECTION>` | - | Show one connection to a Git host. |
| `gradient project git-host-connection edit <CONNECTION>` | Flags of `create`, all optional | Edit the connection. Omitted flags keep their current values. Leave `--access-token`, `--base-url` or `--allowed-ips` empty to clear the value. |
| `gradient project git-host-connection delete <CONNECTION>` | - | Delete a connection. Tasks that use the connection stop reporting commit status. |
| `gradient project git-host-connection deliveries <CONNECTION>` | - | List the latest 50 webhook deliveries for the connection, newest first. |
| `gradient task select <TASK>` | - | Choose the task that later commands target, within the selected project |
| `gradient task show` | - | Display the selected task, its last evaluation and its builds |
| `gradient task log` | - | Stream logs from the last evaluation of the selected task |
| `gradient task create` | `--name <NAME>`, `--display-name <DISPLAY_NAME>`, `--description <DESCRIPTION>`, `--repository <REPOSITORY>`, `--wildcard <WILDCARD>`, `--branch <BRANCH>`, `--trigger <TRIGGER>`, `--no-default-trigger` | Create a task in the selected project. Fields left empty open in `$EDITOR`. `--trigger` can take `polling`, `cron=<EXPRESSION>`, `push`, or `pr`. The flag can occur multiple times. Omit `--trigger` to give the task one polling trigger for `--branch`. Add `--no-default-trigger` to create the task with no triggers. |
| `gradient task duplicate` | `--name <NAME>`, `--display-name <DISPLAY_NAME>`, `--branch <BRANCH>` | Copy the selected task to a new task with its settings, triggers and actions. All triggers of the copy use `--branch`. |
| `gradient task list` | - | List all tasks in the selected project |
| `gradient task edit` | `--new-name <NEW_NAME>`, `--display-name <DISPLAY_NAME>`, `--description <DESCRIPTION>`, `--repository <REPOSITORY>`, `--wildcard <WILDCARD>` | Change details of the selected task |
| `gradient task delete` | `--yes` | Remove the selected task |
| `gradient task evaluate` | - | Trigger a new evaluation of the selected task |
| `gradient task transfer <PROJECT>` | - | Move the selected task to another project. |
| `gradient task repository show` | - | Show the task repository, its Git host connection, **Report commit status** settings, webhook URL, deploy key, last event, latest commit status post and connections that the task can use. |
| `gradient task repository set-connection` | `--connection <NAME_OR_ID>`, `--no-connection`, `--report-status <BOOL>` | Select the connection to the Git host for a task and turn commit status reports on or off. `--no-connection` removes the connection from the task. Omitted flags keep the current values. |
| `gradient task repository check-commit-status` | - | Post a pending test commit status on the last evaluated commit through the task connection. |
| `gradient task repository install-deploy-key` | - | Add the project's SSH key as a read-only deploy key on the Git host through the task connection. |
| `gradient task repository install-webhook` | - | Create a webhook on the Git host. The webhook can send repository events to the task's connection. |
| `gradient task action list` | - | List actions of the selected task. |
| `gradient task action show <ACTION>` | - | Print one action by name or UUID |
| `gradient task action create --name <NAME> --config <JSON>` | `--events <EVENTS>`, `--inactive` | Create an action from JSON settings. Command output: the token for the new send_web_request action, one time only. |
| `gradient task action edit <ACTION>` | `--name <NAME>`, `--config <JSON>`, `--events <EVENTS>`, `--active <BOOL>` | Change the action. Omitted flags keep their current values. |
| `gradient task action delete <ACTION>` | - | Delete action |
| `gradient task action deliveries <ACTION>` | `--limit <LIMIT>`, `--offset <OFFSET>` | List past deliveries of an action |
| `gradient task action delivery <ACTION> <DELIVERY>` | - | Print one delivery, with request body and response body |
| `gradient task action test <ACTION>` | - | Run an action right away with a test event. Actions for the Git host only check the connection. |
| `gradient task flake-input list` | - | List the flake input overrides of the selected task |
| `gradient task flake-input show <INPUT>` | - | Print one override by input name or UUID. |
| `gradient task flake-input create <INPUT_NAME>` | `--url <URL>` | Override the flake input. Omit `--url` to update the input and keep the original URL. |
| `gradient task flake-input edit <INPUT>` | `--new-name <NEW_NAME>`, `--url <URL>`, `--clear-url` | Rename the override or change its URL. |
| `gradient task flake-input delete <INPUT>` | - | Delete override |
| `gradient task trigger list` | - | List the triggers of the selected task. |
| `gradient task trigger show <TRIGGER>` | - | Print one trigger |
| `gradient task trigger create --config <JSON>` | `--inactive` | Create Trigger from JSON Configuration |
| `gradient task trigger edit <TRIGGER>` | `--config <JSON>`, `--active <BOOL>` | Change the trigger. Omitted flags keep their current values. |
| `gradient task trigger delete <TRIGGER>` | - | Delete a trigger |
| `gradient task trigger test <TRIGGER>` | - | Fire the trigger now against the current head commit of the task. |
| `gradient task trigger default <TRIGGER>` | - | Set the trigger to default. Manual evaluations use that branch. |

- `project create`, `task create`, `task duplicate`, `cache create`, `team create`, and `task evaluate` print the UUID of the new item. Pass `--json` to print `{"id", "name", "url"}` instead. Find the web page of the new item at `url`.
- `project show`, `project create`, and `task create` also list missing prerequisites in text mode. See `Wait for a cache subscription before evaluation: gradient project cache add <cache>` for an example.
- Choose `instance`, `team/<name>`, or `project/<name>` for `--owner`. The default is the selected project. Use a connection name or UUID within that owner for `<CONNECTION>`. See [Git Host Connections](../guides/gitea.md#git-host-connections).

## Builds and Evaluations

| Command | Options | Description |
|---|---|---|
| `gradient build [TARGET]` | `--system <SYSTEM>`, `--background`, `--quiet`, `--no-link`, `--override-input <INPUT> <FLAKE>` | Submit a build request from the current git repository |
| `gradient run [TARGET]` | `--no-shell`, `--cpus <CPUS>`, `--memory <MB>`, `--network`, `--labels <LABELS>`, `--timeout <TIMEOUT>`, `--system <SYSTEM>`, `--attach <JOB_ID>`, `-L, --forward <[LOCAL:]PORT>` | Run the flake program on an exec worker and stream the program output to this terminal, see [Run a Program from the CLI](../exec/run.md) |
| `gradient watch <EVALUATION>` | - | Show live build logs and status of an evaluation in a full-screen view. Print a status line like `gradient evaluation wait` and exit if there's no terminal or `--json`. |
| `gradient logs <EVALUATION>` | `--exit-code` | Print complete logs for every build in an evaluation. `--exit-code` can exit with the evaluation result, like `gradient evaluation wait`. |
| `gradient download [FLAKE_REF]` | `--evaluation <EVALUATION>`, `--products <PRODUCTS>`, `--out <OUT>` | Download evaluation artefacts |
| `gradient builds graph <ID>` | `--interactive` | Show a build's dependency graph |
| `gradient builds log <ID>` | `--interactive`, `--lines <LINES>`, `--search <SEARCH>`, `--case` | View a build's log |
| `gradient builds retry <ID>` | - | Queue a failed, timed-out, aborted or dependency-failed build again, with the builds that failed only because of it |
| `gradient builds abort <ID>` | - | Abort the builds in created, queued, or running state for the evaluation in progress |
| `gradient builds prioritize <ID>` | - | Assign builds and their unresolved dependencies before other work. Superuser only. |
| `gradient builds closure <ID>` | `--runtime-closure` | Show the dependency closure of a build and the size of each store path. `--runtime-closure` shows the paths that the output references at runtime instead. |
| `gradient builds sbom <ID>` | `--output-file <PATH>` | Save the CycloneDX SBOM for the runtime closure of the build. Default file: the name from the server in the current directory. |
| `gradient builds follow <ID>` | - | Print live status and progress events for a build. With `--json`, each event is one line of JSON. |
| `gradient evaluation retry <EVALUATION>` | - | Start a new evaluation of the same commit and rebuild the failed builds. Print the UUID of the new evaluation. |
| `gradient evaluation abort <EVALUATION>` | - | Abort a running evaluation |
| `gradient evaluation prioritize <EVALUATION>` | - | Distribute all builds of the running evaluation ahead of other jobs. Superuser only. |
| `gradient evaluation summary <EVALUATION>` | - | Show failed builds with the packages that they block, the attributes that failed evaluation and the packages that were fixed since the last evaluation. |
| `gradient evaluation report <EVALUATION>` | `--output-file <PATH>`, `--keep-real-identities`, `--anonymize-package-names`, `--exclude-failed-build-logs`, `--include-worker-fleet`, `--include-exec-jobs` | Save an SQLite diagnostic report for a bug report. Default settings anonymize identifying information and include failed build logs. Only a user with permission to manage workers can use the `--include-worker-fleet` flag. |
| `gradient evaluation closure <EVALUATION>` | `--runtime-closure` | Display the dependency closure for all entry points, with the size of each store path. `--runtime-closure` can show the runtime closure instead. |
| `gradient evaluation show-flake-graph <EVALUATION>` | - | List the flake outputs that the evaluation reached, with their type and derivation path. |
| `gradient evaluation follow <EVALUATION>` | `--exit-code` | Emit live events from an evaluation until completion. Pass `--json` to get one line of JSON per event. Pass `--exit-code` to exit with the evaluation result, as in `gradient evaluation wait`. Use `gradient watch` for full-screen display. |
| `gradient evaluation wait <EVALUATION>` | `--timeout <SECONDS>` | Wait for evaluation completion. Print one line on each status change. Exit with 0 on completion, 1 on failure, 4 on cancellation, or 5 for a timeout from `--timeout` before evaluation completion. |
| `gradient eval <PATTERN>...` | `--workers <N>`, `--max-rss <MiB>`, `--max-memory <MiB>` | Evaluate a flake to derivations locally, a JSON line per attribute. Only in `gradient-cli-full`, see [below](#local-evaluation) |

- `build` and `run` no longer accept the `-p` short flag. Use the global `--project <NAME>` instead.
- `<EVALUATION>` can receive an evaluation UUID, `project/task` (the latest evaluation of the task), `project/task@<commit>` (a full or short commit hash), or a link from the Web UI. Build `<ID>` can receive a build UUID, or a link to a log page with `?build=<id>` appended. `--task` can also receive a link to a task page.
- Exit codes 0, 1, 2, 3, 4 and 5 map to success, server error or evaluation failure, incorrect usage, not logged in, evaluation abort and timeout. A script designed to accept an aborted evaluation can check for 4, for example ``gradient evaluation wait acme/web || [ $? -eq 4 ]``.

## Pull Requests

| Command | Options | Description |
|---|---|---|
| `gradient pr open [PATH]...` | `--title <TITLE>`, `--body <FILE>`, `--branch <NAME>`, `--base <REF>` | Open or update a pull request from the working tree changes of the listed paths, all paths by default |

- Start commit: `GRADIENT_COMMIT` inside an exec job, else the merge base of `HEAD` and `origin/<base>`.
- Pull requests use the connection to the Git host that the `open_pr` action on the selected task names. See [Open Pull Requests](../exec/pull-requests.md).

## Caches

| Command | Options | Description |
|---|---|---|
| `gradient cache create` | `--name <NAME>`, `--display-name <DISPLAY_NAME>`, `--description <DESCRIPTION>`, `--priority <PRIORITY>`, `--max-storage-gb <MAX_STORAGE_GB>` | Create a cache. Fields left empty open in `$EDITOR` for completion |
| `gradient cache list` | - | List all caches owned by the current user |
| `gradient cache edit <NAME>` | `--display-name <DISPLAY_NAME>`, `--description <DESCRIPTION>`, `--priority <PRIORITY>`, `--max-storage-gb <MAX_STORAGE_GB>` | Change settings of an existing cache |
| `gradient cache delete <NAME>` | `--yes` | Remove a cache |
| `gradient cache show <NAME>` | - | Print the public key of a cache |
| `gradient cache rotate-key <NAME>` | - | Replace the cache signing key and print the new public key. Store paths keep the old signature until the next rotation. |
| `gradient cache install-netrc --server <SERVER> --cache <CACHE>` | `--server <SERVER>`, `--token <TOKEN>`, `--cache <CACHE>`, `--netrc-file <NETRC_FILE>` | Write a netrc entry for a cache from an existing API key. The server is not contacted. Run as root to write `/etc/nix/netrc` |
| `gradient cache nar list <CACHE>` | `--hash <HASH>`, `--package <PACKAGE>`, `--sort <SORT>`, `--order <ORDER>`, `--pinned <PINNED>`, `--page <PAGE>`, `--per-page <PER_PAGE>`, `--interactive` | List the NARs stored in a cache |
| `gradient cache nar show <CACHE> <HASH>` | - | Print the full metadata of one NAR |
| `gradient cache nar delete <CACHE> <HASH>` | `--yes` | Remove a NAR from a cache |
| `gradient cache nar pin <CACHE> <HASH>` | `--include-referenced-paths` | Pin a NAR so garbage collection for the cache can keep it regardless of `gc.narTtlHours` or retained evaluations and `--include-referenced-paths` can also pin all paths in this NAR if they exist in the cache |
| `gradient cache nar unpin <CACHE> <HASH>` | `--include-referenced-paths` | Remove a pin from a NAR. Use `--include-referenced-paths` to unpin all paths in its closure too |
| `gradient cache nar stats <CACHE>` | - | Print aggregate statistics over all NARs in a cache |
| `gradient cache upload <CACHE> [PATHS]...` | `--nar-file <NAR_FILE>`, `--narinfo <NARINFO>`, `--no-closure` | Upload one or more NARs to a cache |
| `gradient cache enable <NAME>` | - | Enable a cache that was disabled. |
| `gradient cache disable <NAME>` | - | Turn off the cache. The Nix client and builds no longer use the cache. |
| `gradient cache publish <NAME>`, `gradient cache unpublish <NAME>` | - | Allow cache access without login, or make the cache private again. |
| `gradient cache subscribers <NAME>` | - | List projects that subscribe to the cache. |
| `gradient cache user list\|add\|edit\|remove <CACHE> [USER] [ROLE]` | - | List, invite, change roles for, or remove cache users. |
| `gradient cache invitation list\|revoke <CACHE> [USER]` | - | List or revoke pending invitations in the cache. |
| `gradient cache role list\|show\|create\|edit\|delete <CACHE>` | `--name <NAME>`, `--role-permission-names <PERMISSIONS>` | Manage custom roles. The `list` outputs also include the permissions that a role can use. |
| `gradient cache team list\|add\|edit\|remove <CACHE> [TEAM] [ROLE]` | - | Manage the teams that can access the cache and their roles. |
| `gradient cache request list\|approve\|deny <CACHE> [PROJECT]` | - | List, approve, or deny requests from projects to subscribe to the cache. |
| `gradient cache upstream list\|add\|edit\|delete\|test <CACHE> [ID]` | `--display-name <NAME>`, `--internal-cache-name <CACHE>`, `--url <URL>`, `--public-key <KEY>`, `--http1-only`, `--remote-cache-name <CACHE>`, `--api-key <KEY>`, `--cache-access-mode <MODE>`, `--active <BOOL>` | Manage upstream caches. `add` can take `--internal-cache-name` for caches on this server, `--url` with `--public-key` for Nix binary caches and `--url` with `--remote-cache-name` for another Gradient server. `test` can probe HTTP/1.1 and HTTP/2. |
| `gradient cache webhook <VERB> <CACHE>` | see [Webhooks](#webhooks) | Manage webhooks for the cache. See [Webhooks](#webhooks) |

## Workers

| Command | Options | Description |
|---|---|---|
| `gradient worker register --display-name <DISPLAY_NAME>` | `--url <URL>`, `--enable-network <BOOL>`, `--enable-secrets <BOOL>`, `--enable-exec <BOOL>`, `--enable-eval <BOOL>`, `--enable-build <BOOL>` | Register a worker in the chosen project. Generate a worker ID and a token on the server, then print a NixOS configuration and a peers line. |
| `gradient worker list` | - | Show every worker in the selected project |
| `gradient worker delete <WORKER_ID>` | `--yes` | Remove a worker from the selected project |
| `gradient worker metrics <WORKER_ID>` | - | Print worker load samples, connections and dispatched jobs |
| `gradient worker check <WORKER_ID>` | - | Check the connection of the worker and the permission to work on the selected project. |

## Exec Jobs

Exec jobs are programs that `gradient run` can launch on exec workers. These commands inspect and control a job by its ID.

| Command | Options | Description |
|---|---|---|
| `gradient exec-job list <EVALUATION>` | - | List the exec jobs for an evaluation |
| `gradient exec-job show\|logs\|cancel <JOB>` | - | Show details of an exec job, output logs, or cancel the job. |
| `gradient exec-job artifact list <JOB>`, `gradient exec-job artifact download <JOB> <PATH>` | `--node-index <N>`, `--output-file-path <FILE>` | List the output files of a job, or download one of them. Keep a path with `/` as one argument. |
| `gradient exec-job keep-vm-running <JOB> on\|off` | - | Specify whether to leave the VM running after program exit, or to stop it. |
| `gradient exec-job console <JOB>` | `--node-index <N>` | Connect the terminal to the VM serial console. Use `~.` at the start of a line to detach without stopping the job. The console can reject `--json`. |
| `gradient exec-job pull-requests <JOB>` | - | List the pull requests that the job opened. |

## Administration

These commands act on the whole instance. They need a superuser account. AI agents can't use them through `gradient mcp`.

| Command | Options | Description |
|---|---|---|
| `gradient administration state` | `--export-format <nix\|json>` | Output the live configuration as `services.gradient.state`. The fields in the credentials file stay empty. |
| `gradient administration drain on\|off` | - | Turn drain on or off. Workers receive no new jobs while drain is active. |
| `gradient administration deep-gc` | - | Start deep garbage collection for the database and storage. Print the task ID. |
| `gradient administration task list\|show [TASK_ID]` | - | List the administration tasks, or show one task (for example, to track deep garbage collection). |
| `gradient administration webhook <VERB>` | see [Webhooks](#webhooks) | Manage webhooks across the entire instance. See [Webhooks](#webhooks) for details. |
| `gradient administration worker list\|show\|drain\|undrain [WORKER_ID]` | - | List registered workers with team, project and connection status, or show a single worker. Drain a worker to finish running jobs and block new jobs, or end the drain. |
| `gradient administration config` | - | Show all server options with their values and sources. Secret values stay hidden. |
| `gradient administration storage` | - | Display free disk space and the stored size of each cache, project, and task. |
| `gradient administration gc` | - | Start a GC round immediately, without waiting for the next interval. |

## Webhooks

`gradient project webhook`, `gradient team webhook`, `gradient cache webhook`, and `gradient administration webhook` share these verbs. Team and cache webhooks take a team name or a cache name before other arguments.

| Command | Options | Description |
|---|---|---|
| `list`, `show <ID>` | - | List webhooks with the result of the last delivery, or print a single webhook. |
| `create` | `--name <NAME>`, `--url <URL>`, `--event <PATTERN>`, `--active <BOOL>` | Create a webhook and output its signing secret once. Repeat `--event` for multiple patterns. Omit `--event` to send all events. |
| `edit <ID>` | `--name <NAME>`, `--url <URL>`, `--event <PATTERN>`, `--all-events`, `--active <BOOL>` | Change a webhook. `--event` replaces all event patterns |
| `delete <ID>` | `--yes` | Remove a webhook |
| `deliveries <ID>`, `delivery <ID> <DELIVERY_ID>` | `--limit <N>`, `--offset <N>` | List past deliveries, or print one delivery with its request body and response body. |
| `rotate-secret <ID>`, `ping <ID>` | - | Replace the signing secret, or send a ``webhook.ping`` event. |

- AI agents can run webhook commands for projects, teams and caches through `gradient mcp --control`. The MCP server rejects `create` and `rotate-secret`, since both commands print the signature secret.

## Teams

| Command | Options | Description |
|---|---|---|
| `gradient team list` | - | List your teams with your role in each |
| `gradient team show <TEAM>` | - | Display the settings of a team |
| `gradient team create` | `--name <NAME>`, `--display-name <DISPLAY_NAME>` | Create a team and become its admin. Fields left empty open in `$EDITOR` |
| `gradient team edit <TEAM>` | `--display-name <DISPLAY_NAME>`, `--new-project-users <BOOL>`, `--new-project-workers <BOOL>`, `--new-project-role <ROLE>`, `--oidc-group <GROUP>`, `--scim-group <GROUP>` | Change the settings of a team. Only superusers change the groups and the new project settings |
| `gradient team delete <TEAM>` | `--yes` | Remove a team with its grants, grant requests, invitations and workers |
| `gradient team members list <TEAM>` | - | Show the members of a team |
| `gradient team members add <TEAM> <USER>` | `--role <ROLE>` | Invite a user, `member` by default. A superuser adds the user right away |
| `gradient team members edit <TEAM> <USER>` | `--role <ROLE>` | Change the role of a member |
| `gradient team members remove <TEAM> <USER>` | - | Remove a member |
| `gradient team pending-invitations list <TEAM>` | - | Show the invitations not accepted yet |
| `gradient team pending-invitations withdraw <TEAM> <USER>` | - | Withdraw the invitation of a user |
| `gradient team grant-requests list <TEAM>` | - | Show the projects and caches asking to grant the team access |
| `gradient team grant-requests approve <TEAM> <REQUEST_ID>` | - | Approve a grant request |
| `gradient team grant-requests reject <TEAM> <REQUEST_ID>` | - | Reject a grant request |
| `gradient team projects-and-caches <TEAM>` | - | Show the projects and caches that grant the team access |
| `gradient team worker list <TEAM>` | - | Show the workers of a team |
| `gradient team worker register <TEAM>` | `--display-name <DISPLAY_NAME>`, `--url <URL>`, `--enable-network <BOOL>`, `--enable-secrets <BOOL>`, `--enable-exec <BOOL>`, `--enable-eval <BOOL>`, `--enable-build <BOOL>` | Add a worker that builds for every project granting the team its workers |
| `gradient team worker edit <TEAM> <WORKER_ID>` | `--display-name <DISPLAY_NAME>`, `--active <BOOL>`, `--draining <BOOL>`, `--enable-network <BOOL>`, `--enable-secrets <BOOL>`, `--enable-exec <BOOL>`, `--enable-eval <BOOL>`, `--enable-build <BOOL>` | Edit a team worker. Disabling it can abort running jobs. Use `--draining true` to let running jobs complete and to stop new job submission. |
| `gradient team worker delete <TEAM> <WORKER_ID>` | `--yes` | Remove a team worker |
| `gradient team worker missing-worker-capabilities <TEAM>` | `--projects <PROJECTS>` | Show what waiting evaluations need that no connected worker offers |
| `gradient team webhook <VERB> <TEAM>` | see [Webhooks](#webhooks) | Manage team webhooks. See [Webhooks](#webhooks). |

## Tools

| Command | Options | Description |
|---|---|---|
| `gradient generate apikey` | - | Create an API token plus the digest needed for a declarative `api_keys.<name>.key_file`, see [API Key Files](../concepts/declarative-state.md#api-key-files) |
| `gradient search [QUERY]` | `--limit <LIMIT>` | Search readable projects, tasks, caches, commit records and store paths. List starred items if the query is empty. |
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

Artifacts from different entry points that share a file name go into separate folders. Folder names derive from the attribute of each entry point (for example `packages.x86_64-linux.app/`).

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
