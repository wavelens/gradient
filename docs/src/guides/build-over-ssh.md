# Build over SSH

The Gradient cache as an `ssh-ng://` store. Paths go in and out with `nix copy`. Builds from `nixos-rebuild --build-host` take place on the CI workers.

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

**Settings** -> **SSH Keys** -> **Add SSH Key** can take an OpenSSH public key line.

```sh
ssh-keygen -t ed25519 -f ~/.ssh/gradient -C laptop
cat ~/.ssh/gradient.pub
```

- Keys belong to their owner only.
- The SSH user name is the project name. The key can open all projects of its user.

## 3. Point SSH at the Server

```nix
# configuration.nix of the client
programs.ssh.extraConfig = ''
  Host ci.example.com
    Port 2222
    User myproject # (1)!
    IdentityFile /root/.ssh/gradient
'';
```

1.  The project name, used by store URLs without `myproject@`.

Substituters connect through the Nix daemon as `root`. Plain `nix copy` and `nixos-rebuild` are using the calling user's SSH setup.

## 4. Use the Store

| Use | Command |
|---|---|
| Substituter | `substituters = ssh-ng://myproject@ci.example.com` plus the cache's [public key](share-a-cache.md) |
| Copy out | `nix copy --from ssh-ng://myproject@ci.example.com /nix/store/...-hello` |
| Copy in | `nix copy --to ssh-ng://myproject@ci.example.com ./result` |
| Build host | `nixos-rebuild switch --build-host ssh-ng://myproject@ci.example.com` |
| Remote store | `nix build --eval-store auto --store ssh-ng://myproject@ci.example.com .#hello` |

- Gradient can answer reads from the project's subscribed caches.
- Copied paths land in the project's caches with a signature, like build outputs.
- Build requests from a user share an evaluation in the project's **Build Requests** task, across SSH connections.
- New requests add their entry points to the running evaluation of their user.
- Requests after the evaluation finished start a new evaluation.
- Builds keep running after a disconnect. Evaluations end with their last build.
- Missing systems appear in the Nix output while no connected worker can build them.
- Aborts of the evaluation stop all unfinished builds with an error, e.g. after [5 minutes without a matching worker](../concepts/evaluations-and-builds.md).
- Build logs stream back like logs of local builds. Log lines of `nix build -L` start with the package name.

## 5. Add a Remote Builder

The `client` module can register Gradient as a Nix remote builder. The CI workers then take over builds for the listed systems.

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
  project = "myproject";
  identityFile = "/root/.ssh/gradient"; # (1)!
  systems = [ "x86_64-linux" "aarch64-linux" ];
  supportedFeatures = [ "big-parallel" "kvm" "nixos-test" ];
};
```

1.  The module can write the SSH settings of step 3 for this host. The Nix daemon can read the key as `root`.

## Verify Deployment

```sh
nix store ping --store ssh-ng://myproject@ci.example.com
```

Expected output: the store URL and `Trusted: 0`. Build requests appear in the UI under **Build Requests**.

## Limits

- Gradient is not supporting `ssh://`. Use the `ssh-ng://` prefix with `--build-host`.
- Gradient is not accepting content-addressed derivations.
- Pass `--eval-store auto` to `nix build --store`. Gradient is not taking evaluation writes.

## Next Steps

- [Share a Cache](share-a-cache.md): substituter URL and public key
- [Build Before Pushing](build-before-push.md): building uncommitted changes with `gradient build`
- [Configuration](../reference/configuration.md#ssh): all `ssh` options
