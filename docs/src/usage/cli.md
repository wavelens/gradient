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

### Builds

```sh
# Build a derivation directly (remote build)
gradient build <derivation-path>
gradient build <derivation-path> --organization myorg

# Download build artifacts
gradient download --build-id <uuid>
gradient download --build-id <uuid> --filename output.tar
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
