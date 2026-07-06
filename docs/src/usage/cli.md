# CLI

The Gradient CLI provides command-line access to all server functionality. It communicates with the same REST API as the web interface.

## Installation

### Via Nix Flake (NixOS)

```nix
{
  inputs.gradient.url = "github:wavelens/gradient";

  outputs = { self, nixpkgs, gradient, ... }:
  let
    pkgs = import nixpkgs {
      system = "x86_64-linux";
      overlays = [ gradient.overlays.gradient-cli ];
    };
  in {
    nixosConfigurations.yourhostname = pkgs.lib.nixosSystem {
      system = "x86_64-linux";
      modules = [
        ./configuration.nix
        { environment.systemPackages = [ pkgs.gradient-cli ]; }
      ];
    };
  };
}
```

### Run Without Installing

```sh
nix run github:wavelens/gradient#gradient-cli -- --help
```

## Configuration

Log in and point the CLI at your Gradient server in one step:

```sh
gradient login https://gradient.example.com
```

Passing the URL sets it as the configured server, so a separate `gradient config server` is no longer needed. On a successful first login the CLI selects your organization automatically when you belong to exactly one, and otherwise lists them so you can pick with `gradient organization select <name>`.

By default this opens your browser to authorize the CLI session, which is what you want for interactive use and works the same when the Gradient server is configured for OIDC-only login. Pass `--no-browser` to print the URL instead - useful when running over SSH on a headless machine, where you can open the URL on your laptop. The browser flow asks you to confirm a short code that the CLI also prints, then issues a 30-day session token.

For unattended scripts you can still pass credentials directly:

```sh
gradient login https://gradient.example.com --username alice --password "$PASSWORD"
```

The server URL can also be set on its own with `gradient config server <url>`, and `gradient organization select` requires a valid login and only accepts an organization you belong to. Either way, the resulting token is stored in the local configuration file (`~/.config/gradient/config`).

### Self-signed certificates

The CLI trusts the OS certificate store in addition to the bundled Mozilla
CA roots, so self-hosted instances served behind a private CA work the same
way `curl` does - install the CA in your system trust store (for example
`update-ca-certificates` on Debian/Ubuntu, `trust anchor` on Fedora, or by
adding it to `/etc/ssl/certs` on NixOS via `security.pki.certificateFiles`)
and `gradient login` will pick it up. A `transport error` from `gradient
login` against a self-hosted server typically means the CA has not been
installed system-wide yet.

## Commands

### Authentication

| Command | Description |
|---|---|
| `gradient register` | Register a new user account |
| `gradient login` | Log in and store an auth token |
| `gradient logout` | Clear the stored auth token |
| `gradient info` | Print current user information |
| `gradient status` | Check connectivity to the server |

### Organizations

```sh
gradient organization list
gradient organization create
gradient organization delete <name>
```

### Projects

```sh
gradient project list
gradient project create
gradient project delete <name>
gradient project eval <name>          # Trigger a new evaluation
```

### Caches

Cache CRUD:

```sh
gradient cache list
gradient cache add
gradient cache remove <name>
```

NAR management (list, inspect, delete, and upload cached store paths inside a
cache):

```sh
gradient cache nar list <cache> [--hash <prefix>] [--package <substring>] \
  [--sort created_at|nar_size|last_fetched_at] [--order asc|desc] \
  [--page N] [--per-page N]
gradient cache nar show <cache> <hash>
gradient cache nar delete <cache> <hash> [-y]
gradient cache nar stats <cache>

# Upload a pre-dumped NAR (no Nix required)
gradient cache upload --nar-file <file.nar> --narinfo <file.narinfo> <cache>

# Upload from the local Nix store (nix feature only)
gradient cache upload [--full-closure] <store-path>... <cache>
```

Deleting a NAR is ref-counted: if the NAR is signed by more than one cache,
the delete only drops the current cache's signature; the underlying NAR blob
stays. When the last cache holding a NAR drops it, the blob is GC'd
asynchronously and any `derivation_output.is_cached` rows for it flip to
`false`.

Uploading requires the `writeStore` cache permission. The server enforces a
maximum NAR upload size (default 512 MiB, configurable via
`GRADIENT_MAX_NAR_UPLOAD_SIZE`). See [Managing cached NARs](cache-nars.md)
for full upload documentation.

### Build Requests

`gradient build` uploads the current git repository's tracked files to the
server and queues a Nix evaluation against them. No Nix tooling runs on the
client - only the files git tracks are uploaded, addressed by BLAKE3 content
hash so unchanged blobs aren't re-sent across runs. The server materialises
`/nix/store/<hash>-source`, signs it with the org's cache key, and dispatches
an evaluation under a per-org reserved `build-request` project.

```sh
# Inside a git working tree
gradient build                          # eval the project's wildcard target
gradient build .#foo                    # nix-style installable -> packages.<system>.foo
gradient build checks.x86_64-linux.foo  # eval a specific attribute path
gradient build --system x86_64-linux    # system used to expand a bare `.#foo` target
gradient build -b                       # dispatch and print the evaluation UUID, then exit
gradient build --no-link                # skip producing a result symlink/folder
```

The target accepts either gradient's attr-path wildcard syntax (`.`-separated,
with `*`/`#` wildcard segments and `!`-prefixed exclusions, e.g.
`packages.x86_64-linux.#`) or a `nix build`-style installable (`.#foo`). An
installable's flake ref is always the uploaded repo, so `.#foo` is expanded to
`packages.<system>.foo` (`<system>` from `--system` or the host). A pinpointed
target that matches no derivation fails the evaluation with a clear message
instead of completing empty.

Requirements and limits:

- Run from inside a git working tree (bare repos are not supported).
- Only files git tracks are uploaded; untracked files and `.git/` are skipped.
- Total source size is capped by `settings.maxSourceUploadSize` (512 MiB
  default); the source is streamed in bounded chunks, so no single request
  hits the reverse proxy's body limit.
- The default flow streams logs from all queued builds until they complete;
  pass `-b`/`--background` to print only the evaluation UUID and return
  immediately. Pair it with [`gradient watch`](#watching-an-evaluation) to
  follow the build later: `eval=$(gradient build -b); gradient watch "$eval"`.

After a foreground build completes, the CLI produces a `result` for the primary
output (use `--no-link` to skip it):

- A CLI built with the `nix` feature realises the primary output into the local
  Nix store and creates a single GC-rooted `result` symlink to it (like
  `nix build`). The realise wires the org cache in as an extra substituter -
  carrying its signing key, and a temporary `netrc` with the CLI token when the
  cache is private - alongside the user's own substituters, so an output the org
  cache serves and one already reachable locally both resolve. It also packs the
  source NAR locally and uploads it in one shot (`POST /build-requests/source`),
  skipping the per-file blob manifest.
- A CLI without the `nix` feature downloads the primary entry point's build
  products into a `result/` folder (only declared `hydra-build-products` are
  included).

### Watching an evaluation

`gradient watch <evaluation>` opens a live full-screen dashboard for any
evaluation UUID. It shows the evaluation status and elapsed time, a per-build
list with statuses and build times, evaluation messages and errors as they
appear, and a follow-tail log pane that merges every build's output.

```sh
gradient watch 0190f3c2-...   # live dashboard for an evaluation
```

Key bindings: `↑`/`↓` scroll the log, `f` toggle follow-tail, `q`/`Esc` quit.
In `--json` mode the dashboard is skipped and the merged build logs are streamed
as JSON envelopes to stdout instead.

### Builds

`gradient builds` is a top-level command group for inspecting build metadata
after dispatch.

```sh
# Collapsible dependency-graph browser for a specific build
gradient builds graph <build-id> [-i]

# Build log viewer / streamer for a specific build
gradient builds log <build-id> [-i]
```

Without `-i`, `builds graph` prints the node and edge counts to stdout.
Without `-i`, `builds log` streams the log to stdout.

### Interactive mode (`-i` / `--interactive`)

Several commands accept `-i` / `--interactive` to open a full-screen
[ratatui](https://github.com/ratatui/ratatui) TUI instead of plain text
output. The flag is silently ignored in `--json` mode.

| Command | TUI description | Key bindings |
|---|---|---|
| `gradient cache nar list -i` | Scrollable, type-to-filter NAR browser | Type to filter by package or hash; `↑`/`↓` navigate; `Esc` quit |
| `gradient builds graph <id> -i` | Collapsible dependency-graph browser (nix-tree style) | `↑`/`↓` navigate; `Enter`/`Space` expand or collapse a node; `Esc` quit |
| `gradient builds log <id> -i` | Less-style log pager with follow-tail | `↑`/`↓` scroll; `f` toggle follow-tail; `/` search; `Esc` quit |

### Downloading artefacts

```sh
# Interactive picker over the latest evaluation of the selected project
gradient download

# Skip the evaluation picker
gradient download --evaluation <uuid>

# Skip the product picker (comma-separated indices, ranges, or `all`)
gradient download --products all --out ./artefacts
gradient download --products 1,3-5 --out ./artefacts

# Filter by flake-output attribute (leading `#` is optional; comma-separate for multiple)
gradient download '#packages.x86_64-linux.my-app'
gradient download 'packages.x86_64-linux.my-app'
gradient download '#a,#b,#c'
```

The positional attribute argument matches the evaluation's artefact tree by exact `entry_points[].attr` equality. It cannot be combined with `--products`.

### Local evaluation (`gradient eval`)

Evaluate a flake's outputs to derivations locally, like
[`nix-eval-jobs`](https://github.com/nix-community/nix-eval-jobs), using the
same evaluator the Gradient worker runs. It streams one JSON line per resolved
attribute (`attr`, `attrPath`, `drvPath`); a per-attribute failure is reported
in its own line (`{"attr": ..., "error": ...}`) and does not abort the run.

```sh
gradient eval 'packages.x86_64-linux.*'                # current flake (.)
gradient eval .#packages.x86_64-linux.hello            # installable syntax sets the flake
gradient eval 'github:NixOS/patchelf#hydraJobs.*'      # any flake ref before the '#'
gradient eval 'checks.*.*' 'packages.*.*'              # multiple wildcard patterns
```

The flake to evaluate is taken from the part before `#` in a pattern (default:
the current directory); every pattern shares one flake. A local flake ref (`.`,
`./sub`, a relative dir) is resolved to an absolute path, since the Nix C API -
unlike the CLI - only accepts absolute flake paths; a scheme ref (`github:`,
`path:`, `git+…`) is passed through unchanged.

This subcommand is gated behind the `nix` and `eval` cargo features (it pulls in
libnix) and is therefore only shipped by the `gradient-cli-full` package, not the
default lean `gradient-cli`:

```sh
nix run github:wavelens/gradient#gradient-cli-full -- eval 'packages.x86_64-linux.*'
```

### Utilities

```sh
# Generate shell completions
gradient completion bash   >> ~/.bashrc
gradient completion zsh    >> ~/.zshrc
gradient completion fish   >> ~/.config/fish/completions/gradient.fish

# Generate project files
gradient generate <type>
```

Completions are dynamic: besides subcommands and flags, TAB also completes
existing resource names (organizations, projects, workers, and caches) by
querying the server. The resource lookups require a configured server and a
valid login; when offline or logged out they yield nothing instead of erroring.
Project and worker name completion uses the currently selected organization
(`gradient organization select <name>`).

## Global Options

```text
gradient --help      Show help for any command
gradient --version   Print CLI version
gradient --json      Emit machine-readable JSON output
```

### `--json`

Emit machine-readable JSON envelopes mirroring the server's response shape:

- Success: `{"error": false, "message": <data>}`
- Failure: `{"error": true, "message": "<reason>"}`

In `--json` mode all human-readable output is suppressed on stdout (progress messages go to stderr; banners are hidden). Interactive prompts are disabled - missing inputs that would otherwise have been prompted produce an error with exit code 2. For streaming endpoints (e.g. build logs), each chunk is emitted on its own line as a JSON envelope (NDJSON).

Exit codes: `0` success, `1` API/network/IO/decode error, `2` usage/missing argument, `3` unauthorized.

### Workers

```sh
# Register a worker (worker_id must be a UUID v4).
# The worker auto-generates one on first start and writes it to
# /var/lib/gradient-worker/worker-id - use that value here.
gradient worker register <uuid>

# Register with optional URL and pre-generated token
gradient worker register <uuid> --url wss://builder.example.com/proto
gradient worker register <uuid> --token "$(openssl rand -base64 48)"

# List registered workers (shows live connection status)
gradient worker list

# Unregister a worker
gradient worker delete <uuid>
```

When no `--token` is given, the server generates one and prints it once - store it securely. When `--token` is supplied, the token is not echoed back (the server stores only its hash).
