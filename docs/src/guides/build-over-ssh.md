# Build over SSH

The Gradient cache as an `ssh-ng://` store. Paths are copied in and out with `nix copy`. Builds from `nixos-rebuild --build-host` take place on the CI workers.

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

**Settings** -> **SSH Keys** -> **Add SSH Key** can take one OpenSSH public key line.

```sh
ssh-keygen -t ed25519 -f ~/.ssh/gradient -C laptop
cat ~/.ssh/gradient.pub
```

- Keys belong to one user only.
- The SSH user name is the project name. The key can open every project the user is a member of.

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

The Nix daemon will open the connection as `root` for substituters. Plain `nix copy` and `nixos-rebuild` are using the calling user's SSH setup.

## 4. Use the Store

| Use | Command |
|---|---|
| Substituter | `substituters = ssh-ng://myproject@ci.example.com` plus the cache's [public key](share-a-cache.md) |
| Copy out | `nix copy --from ssh-ng://myproject@ci.example.com /nix/store/...-hello` |
| Copy in | `nix copy --to ssh-ng://myproject@ci.example.com ./result` |
| Build host | `nixos-rebuild switch --build-host ssh-ng://myproject@ci.example.com` |
| Remote store | `nix build --eval-store auto --store ssh-ng://myproject@ci.example.com .#hello` |

- Gradient can answer reads from the project's subscribed caches.
- Gradient will sign copied paths into the project's caches, like build outputs.
- One SSH connection will become one evaluation under the project's **Build Requests** task.
- Further build requests on the same connection add entry points to that evaluation.
- The evaluation will stay in building while the connection is open.
- Build logs are streaming back with the package name in front of each line.

## 5. Add a Remote Builder

The `client` module can register Gradient as a Nix remote builder. Nix is then sending builds for the listed systems to the CI workers.

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

1.  The module will write the SSH settings of step 3 for this host. The Nix daemon will read the key as `root`.

## Verify Deployment

```sh
nix store ping --store ssh-ng://myproject@ci.example.com
```

The command will print the store URL and `Trusted: 0`. A build request is visible in the UI under **Build Requests**.

## Limits

- Gradient is not supporting `ssh://` (`nix-store --serve`). `--build-host` must use the `ssh-ng://` prefix.
- Gradient will reject content-addressed derivations.
- `nix build --store` must have `--eval-store auto`. Gradient is not taking evaluation writes.
- A closed connection will abort its unfinished builds.

## Next Steps

- [Share a Cache](share-a-cache.md): substituter URL and public key
- [Build Before Pushing](build-before-push.md): building uncommitted changes with `gradient build`
- [Configuration](../reference/configuration.md#ssh): every `ssh` option
