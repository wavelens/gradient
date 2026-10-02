# Standalone

A complete Gradient on one machine for trying Gradient on a personal repository. Server, web UI, PostgreSQL and a worker share one container or VM.

**Requirements:**

- [Docker](https://docs.docker.com/get-started/get-docker/) on Linux, macOS or Windows, or [Nix](https://nixos.org/download/) on Linux with KVM
- 8 GiB RAM and 20 GiB free disk

## 1. Start Gradient

=== "Docker"

    ```sh
    docker run -dt --name gradient \
      --privileged --cgroupns=host \
      -p 127.0.0.1:8080:80 \
      -v gradient:/var/lib \
      ghcr.io/wavelens/gradient-standalone
    ```

    The container is running systemd and the Nix build sandbox. Both are requiring `--privileged`. `-t` is giving systemd a console for `docker logs`. The port is listening on `127.0.0.1` only. The `gradient` volume is keeping projects, builds and the cache across restarts.

=== "Nix"

    ```sh
    nix run github:wavelens/gradient#standalone
    ```

    A QEMU VM on the terminal, logged in as root. The disk image `gradient.qcow2` in the current directory is keeping projects, builds and the cache across restarts. Stop the VM with `Ctrl-a x`.

The first boot is taking a minute. PostgreSQL is initializing, and Gradient is generating its secrets.

## 2. Log In

Gradient is generating a random admin password on first boot. Every boot is printing the password in this line.

```text
Gradient is running at http://localhost:8080 - log in with admin / <password>
```

=== "Docker"

    ```sh
    docker exec gradient cat /var/lib/gradient-standalone/admin-password
    ```

=== "Nix"

    ```sh
    cat /var/lib/gradient-standalone/admin-password
    ```

Open `http://localhost:8080` and log in as `admin`. Self-registration is disabled.

## 3. Build a Repository

Follow [First Project](first-project.md) from step 2 for a cache, a project and a task. The worker inside the standalone instance is joining every new project on its own.

!!! tip "Private Repositories"
    Add the project's public SSH key from **Settings -> SSH Key** as a deploy key on the Git host.

## Verify Deployment

- `http://localhost:8080` is showing the login page.
- The builds show as completed on the evaluation page after the first evaluation.

## Upgrade

=== "Docker"

    ```sh
    docker pull ghcr.io/wavelens/gradient-standalone
    docker rm -f gradient
    ```

    Then start the container again with the command from step 1. The `gradient` volume is carrying the data over.

=== "Nix"

    ```sh
    nix run github:wavelens/gradient#standalone --refresh
    ```

!!! warning
    The standalone instance is serving plain HTTP with a single worker and no backups. Deploy with the NixOS module from the [Quick Start](quick-start.md) for a team.

## Next Steps

- [First Project](first-project.md): a cache, a project and the first green build
- [Quick Start](quick-start.md): a NixOS deployment with TLS
- [Remote Worker](../guides/remote-worker.md): more build capacity
- [Share a Cache](../guides/share-a-cache.md): use the built outputs on other machines
