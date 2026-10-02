# Build over SSH

The Gradient cache as an `ssh-ng://` store. Paths are copied in and out with `nix copy`, and `nixos-rebuild --build-host` is building on the CI workers.

**Requirements:**

- A project with a cache subscription and a worker, see [First Project](../get-started/first-project.md)
- The `TriggerEvaluation` permission in the project for copying in and building, see [Members and Roles](../ui/members-and-roles.md)

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

**Settings** -> **SSH Keys** -> **Add SSH Key** is taking one OpenSSH public key line.

```sh
ssh-keygen -t ed25519 -f ~/.ssh/gradient -C laptop
cat ~/.ssh/gradient.pub
```

- A key is belonging to one user only.
- The SSH user name is the project name. The key is opening every project the user is a member of.

## 3. Point SSH at the Server

```nix
# configuration.nix of the client
programs.ssh.extraConfig = ''
  Host ci.example.com
    Port 2222
    IdentityFile /root/.ssh/gradient
'';
```

The Nix daemon is opening the connection as `root` for substituters. Plain `nix copy` and `nixos-rebuild` are using the calling user's SSH setup.

## 4. Use the Store

| Use | Command |
|---|---|
| Substituter | `substituters = ssh-ng://myproject@ci.example.com` plus the cache's [public key](share-a-cache.md) |
| Copy out | `nix copy --from ssh-ng://myproject@ci.example.com /nix/store/...-hello` |
| Copy in | `nix copy --to ssh-ng://myproject@ci.example.com ./result` |
| Build host | `nixos-rebuild switch --build-host ssh-ng://myproject@ci.example.com` |
| Remote store | `nix build --store ssh-ng://myproject@ci.example.com .#hello` |

- Gradient is answering reads from the project's subscribed caches.
- Gradient is signing copied paths into the project's caches, like build outputs.
- A build request is becoming one evaluation under the project's **Build Requests** task.
- Build logs are streaming back with the package name in front of each line.

## Verify Deployment

```sh
nix store ping --store ssh-ng://myproject@ci.example.com
```

The command is printing the store URL and `Trusted: 0`. A build request is visible in the UI under **Build Requests**.

## Limits

- Gradient is not supporting `ssh://` (`nix-store --serve`). `--build-host` is needing the `ssh-ng://` prefix.
- Gradient is not supporting Nix's `builders` setting (`--builders ssh-ng://...`).
- Gradient is rejecting content-addressed derivations.
- A closed connection is leaving its builds running.

## Next Steps

- [Share a Cache](share-a-cache.md): substituter URL and public key
- [Build Before Pushing](build-before-push.md): building uncommitted changes with `gradient build`
- [Configuration](../reference/configuration.md#ssh): every `ssh` option
