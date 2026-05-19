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

You will be prompted for your username and password. The token is stored in the local configuration file (`~/.config/gradient/config`).

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

```sh
gradient cache list
gradient cache add
gradient cache remove <name>
```

### Build Requests

`gradient build` uploads the current git repository's tracked files to the
server and queues a Nix evaluation against them. No Nix tooling runs on the
client — only the files git tracks are uploaded, addressed by BLAKE3 content
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

## Global Options

```
gradient --help      Show help for any command
gradient --version   Print CLI version
```

### Workers

```sh
# Register a worker (worker_id must be a UUID v4).
# The worker auto-generates one on first start and writes it to
# /var/lib/gradient-worker/worker-id — use that value here.
gradient worker register <uuid>

# Register with optional URL and pre-generated token
gradient worker register <uuid> --url wss://builder.example.com/proto
gradient worker register <uuid> --token "$(openssl rand -base64 48)"

# List registered workers (shows live connection status)
gradient worker list

# Unregister a worker
gradient worker delete <uuid>
```

When no `--token` is given, the server generates one and prints it once — store it securely. When `--token` is supplied, the token is not echoed back (the server stores only its hash).
