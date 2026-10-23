<!--
SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
SPDX-License-Identifier: AGPL-3.0-only
-->

# Build over SSH

Gradient exposes its cache as an `ssh-ng://` store. `nix copy` moves store paths in and out. CI workers run builds started with `nixos-rebuild --build-host`.

**Requirements:**

- A project with a cache subscription and a worker, see [First Project](../get-started/first-project.md)
- The `TriggerEvaluation` permission on the project, needed for copying paths in and building, see [Members and Roles](../ui/members-and-roles.md)

## 1. Enable the SSH Server

```nix
# configuration.nix
services.gradient.ssh = {
  enable = true;
  openFirewall = true; # (1)!
};
```

1.  Port `2222` by default, see [`ssh`](../reference/configuration.md#ssh) for the host key and listen address.

## 2. Add a Key

Paste an OpenSSH public key line into **Settings** -> **SSH Keys** -> **Add SSH Key**.

Generate a key and print the public part:

```sh
ssh-keygen -t ed25519 -f ~/.ssh/gradient -C laptop
cat ~/.ssh/gradient.pub
```

- Each key has a single owner.
- The SSH user name and the project name share one value. A key can open every project of its user.

## 3. Add the Client Module

The `client` module registers Gradient as a Nix remote builder over SSH. CI workers then take over builds for every system listed in the configuration.

```nix
# flake.nix
nixosConfigurations.laptop = nixpkgs.lib.nixosSystem {
  modules = [
    ./configuration.nix
    gradient.nixosModules.client
  ];
};
```

```nix
# configuration.nix
nix.gradient-ssh = {
  enable = true;
  host = "ci.example.com";
  project = "myproject"; # (1)!
  identityFile = "/root/.ssh/gradient"; # (2)!
  systems = [ "x86_64-linux" "aarch64-linux" ];
  supportedFeatures = [ "big-parallel" "kvm" "nixos-test" ];
};
```

1.  The SSH user name matches the project name. Store URLs work without the `myproject@` prefix.
2.  The Nix daemon must be able to read the key as `root`. Plain `nix copy` and `nixos-rebuild` use the calling user's access to the key instead.

## 4. Use the Store

| Use | Command |
|---|---|
| Substituter | `substituters = ssh-ng://myproject@ci.example.com` plus the cache's [public key](share-a-cache.md) |
| Copy out | `nix copy --from ssh-ng://myproject@ci.example.com /nix/store/...-hello` |
| Copy in | `nix copy --to ssh-ng://myproject@ci.example.com ./result` |
| Build host | `nixos-rebuild switch --build-host ssh-ng://myproject@ci.example.com` |
| Remote store | `nix build --eval-store auto --store ssh-ng://myproject@ci.example.com .#hello` |

- Read requests can be answered from the caches the project subscribed to.
- Copied paths enter the project's caches with a signature, like build outputs.
- All build requests from a user share one evaluation in the project's **Build Requests** task, even across several SSH connections.
- Each new request adds its entry points to the evaluation already running for that user.
- Once the evaluation has finished, further requests start a new one.
- Builds continue after a disconnect. Evaluations finish together with their last build.
- The Nix output lists missing systems as long as no connected worker can build them.
- Aborting an evaluation fails every unfinished build with an error, e.g. after [5 minutes without a matching worker](../concepts/evaluations-and-builds.md).
- Build logs stream back like local build logs. Log lines of `nix build -L` start with the package name.

## Verify Deployment

```sh
nix store ping --store ssh-ng://myproject@ci.example.com
```

Expected output: the store URL and `Trusted: 0`. Build requests appear in the UI under **Build Requests**.

## Limits

- `ssh://` has no support in Gradient. Use `--build-host` with the `ssh-ng://` prefix instead.
- Content-addressed derivations have no support in Gradient.
- Pass `--eval-store auto` to `nix build --store`. Gradient rejects evaluation writes.

## Next Steps

- [Share a Cache](share-a-cache.md): substituter URL and public key
- [Build Before Pushing](build-before-push.md): building uncommitted changes with `gradient build`
- [Configuration](../reference/configuration.md#ssh): all `ssh` options
