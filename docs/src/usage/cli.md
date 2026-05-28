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

Before using the CLI, point it at your Gradient server:

```sh
gradient config server https://gradient.example.com
```

Then log in:

```sh
gradient login
```

By default this opens your browser to authorize the CLI session, which is what you want for interactive use and works the same when the Gradient server is configured for OIDC-only login. Pass `--no-browser` to print the URL instead — useful when running over SSH on a headless machine, where you can open the URL on your laptop. The browser flow asks you to confirm a short code that the CLI also prints, then issues a 30-day session token.

For unattended scripts you can still pass credentials directly:

```sh
gradient login --username alice --password "$PASSWORD"
```

Either way, the resulting token is stored in the local configuration file (`~/.config/gradient/config`).

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

NAR management (list, inspect, delete cached store paths inside a cache):

```sh
gradient cache nar list <cache> [--hash <prefix>] [--package <substring>] \
  [--sort created_at|nar_size|last_fetched_at] [--order asc|desc] \
  [--page N] [--per-page N]
gradient cache nar show <cache> <hash>
gradient cache nar delete <cache> <hash> [-y]
gradient cache nar stats <cache>
```

Deleting a NAR is ref-counted: if the NAR is signed by more than one cache,
the delete only drops the current cache's signature; the underlying NAR blob
stays. When the last cache holding a NAR drops it, the blob is GC'd
asynchronously and any `derivation_output.is_cached` rows for it flip to
`false`. NAR upload from local store is tracked separately under
[issue #261](https://github.com/wavelens/gradient/issues/261).

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
gradient build checks.x86_64-linux.foo  # eval a specific attribute path
gradient build --system x86_64-linux    # override target system (default: org preference)
gradient build --no-stream              # dispatch and exit without tailing logs
```

Requirements and limits:

- Run from inside a git working tree (bare repos are not supported).
- Only files git tracks are uploaded; untracked files and `.git/` are skipped.
- Combined upload size must not exceed **20 MiB** (`MAX_BUILD_REQUEST_SIZE`).
- The default flow streams logs from all queued builds until they complete;
  pass `--no-stream` to return immediately after dispatch.

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

### Utilities

```sh
# Generate shell completions
gradient completion bash   >> ~/.bashrc
gradient completion zsh    >> ~/.zshrc
gradient completion fish   >> ~/.config/fish/completions/gradient.fish

# Generate project files
gradient generate <type>
```

## Global Options

```
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
