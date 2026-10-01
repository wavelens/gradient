# Standalone

A complete Gradient on one machine, for trying Gradient on a personal repository: server, web UI, PostgreSQL and a worker in one container or VM.

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

    The container hosts systemd and the Nix build sandbox, which need `--privileged`; `-t` gives systemd a console for `docker logs`. The port is bound to `127.0.0.1` only. The `gradient` volume keeps projects, builds and the cache across restarts.

=== "Nix"

    ```sh
    nix run github:wavelens/gradient#standalone
    ```

    A QEMU VM on the terminal, logged in as root. The disk image `gradient.qcow2` in the current directory keeps projects, builds and the cache across restarts. `Ctrl-a x` stops the VM.

The first boot takes a minute: PostgreSQL initializes and Gradient generates its secrets.

## 2. Log In

Gradient generates a random admin password on first boot and prints the password on every boot:

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

Follow [First Project](first-project.md) from step 2: a cache, a project and a task for the repository. The worker inside the standalone instance joins every new project on its own.

!!! tip "Private Repositories"
    Add the project's public SSH key from **Settings -> SSH Key** as a deploy key on the Git host.

## Verify Deployment

- `http://localhost:8080` shows the login page.
- After the first evaluation, the builds show as completed on the evaluation page.

## Upgrade

=== "Docker"

    ```sh
    docker pull ghcr.io/wavelens/gradient-standalone
    docker rm -f gradient
    ```

    Then start the container again with the command from step 1. The `gradient` volume carries the data over.

=== "Nix"

    ```sh
    nix run github:wavelens/gradient#standalone --refresh
    ```

!!! warning
    The standalone instance serves plain HTTP with a single worker and no backups. For a team, deploy with the NixOS module: [Quick Start](quick-start.md).

## Next Steps

- [First Project](first-project.md): a cache, a project and the first green build
- [Quick Start](quick-start.md): a NixOS deployment with TLS
- [Remote Worker](../guides/remote-worker.md): more build capacity
- [Share a Cache](../guides/share-a-cache.md): use the built outputs on other machines
