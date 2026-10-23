<!--
SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
SPDX-License-Identifier: AGPL-3.0-only
-->

# Standalone

Standalone mode runs the whole Gradient stack on one machine. This setup suits personal projects and evaluation. Server, web UI, PostgreSQL and worker all live in a single container or VM.

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

    Run the container with `--privileged`. Systemd and the Nix build sandbox both run inside it and need that flag. The boot output does not appear in `docker logs`. Step 2 shows the password in its tabs. The port binds to `127.0.0.1` only. The `gradient` volume stores projects, the cache and every build across a restart.

=== "Nix"

    ```sh
    nix run github:wavelens/gradient/latest#standalone
    ```

    This runs a QEMU VM in the terminal with a root login. Projects, the cache and every build persist in the disk image `gradient.qcow2` in the current directory across a restart. Press `Ctrl-a x` to stop the VM. The run script can rebuild the image of the VM's Nix store before every boot. A startup can take longer than a plain VM.

??? note "Image Provenance"
    Release images come with a signed build provenance and an SBOM. The GitHub CLI can verify the provenance against the repository.

    ```sh
    gh attestation verify oci://ghcr.io/wavelens/gradient-standalone:latest -R wavelens/gradient
    ```

The first boot can take a minute. PostgreSQL has to initialize, and Gradient generates its secrets.

## 2. Log In

On first boot, Gradient creates a random password for the `admin` account. Each boot prints that password in this line:

```text
Gradient is running at http://localhost:8080 - log in with admin / <password>
```

To read the current password directly from disk, run one of these commands.

=== "Docker"

    ```sh
    docker exec gradient cat /var/lib/gradient-standalone/admin-password
    ```

=== "Nix"

    ```sh
    cat /var/lib/gradient-standalone/admin-password
    ```

Then open `http://localhost:8080` and sign in as `admin`. New users cannot register themselves.

## 3. Build a Repository

Set up a cache, a project and a task by following [First Project](first-project.md) from step 2 onward. The standalone instance's worker joins each new project automatically.

!!! tip "Private Repositories"
    On the Git host, register the project's public SSH key from **Settings -> SSH Key** as a deploy key.

## Verify Deployment

- The login page loads at `http://localhost:8080`.
- The evaluation page lists the builds as completed once the first evaluation has run.

## Upgrade

=== "Docker"

    ```sh
    docker pull ghcr.io/wavelens/gradient-standalone
    docker rm -f gradient
    ```

    Start the container again with the command from step 1. All data stays in the `gradient` volume.

=== "Nix"

    ```sh
    nix run github:wavelens/gradient/latest#standalone --refresh
    ```

!!! warning
    The standalone instance uses plain HTTP, a single worker and no backups. To run it for a team, deploy with the NixOS module from the [Quick Start](quick-start.md).

## Next Steps

- [First Project](first-project.md): set up a project, add a cache, and run the first successful build
- [Quick Start](quick-start.md): deploy on NixOS with TLS enabled
- [Remote Worker](../guides/remote-worker.md): add extra build capacity
- [Share a Cache](../guides/share-a-cache.md): make build outputs available on other machines
