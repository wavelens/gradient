# Roadmap

Upcoming releases and their features.

<div class="timeline" markdown>

<div class="timeline-item now" markdown>

## v2.0.0

The first release with a stability pledge.

<div class="grid cards" markdown>

-   :material-shield-check: **Stability Pledge**

    Stable, professional releases become a commitment. Each release is shipping migrations for the NixOS module options, the PostgreSQL schema, cache storage, the API and the worker protocol.

-   :material-office-building: **Organizations**

    A new level above projects. One organization is holding the projects, members and workers of a team.

-   :material-database-sync: **Storage Migrations**

    Changes to the S3 and file layout migrate existing caches in place. An interrupted migration can resume after a restart.

-   :material-broom: **Continuous Deep GC**

    Deep garbage collection is running slowly in the background at all times. Checkpoints let each pass resume where the last one stopped.

-   :material-server-network: **Cluster Jobs**

    Jobs that run on several workers at once. Gradient is allocating all members together. Jobs needing a fast interconnect get all members from one [zone](concepts/workers.md#zones).

-   :material-console-network: **SSH Builds**

    An [`ssh-ng://` store](guides/build-over-ssh.md) for every project. `nixos-rebuild --build-host` and `nix copy` talk straight to the CI workers and caches.

-   :material-palette: **Corporate Design**

    Own logo for web interface.

</div>

</div>

<div class="timeline-item next" markdown>

## v2.1.0

Faster evaluation of large flakes.

<div class="grid cards" markdown>

-   :material-graph: **Multi-Node Evaluations**

    One evaluation split across several workers. Large flakes like nixpkgs or fleets of NixOS hosts finish in a fraction of the time.

</div>

</div>

<div class="timeline-item later" markdown>

## v2.2.0

Jobs beyond the Nix sandbox.

<div class="grid cards" markdown>

-   :material-play-network: **Runner Workers**

    Workers that build outside the sandbox. Integration tests with network access, real hardware or deployment credentials.

</div>

</div>

<div class="timeline-item later" markdown>

## v3.0.0

Gradient without a single point of failure.

<div class="grid cards" markdown>

-   :material-server-plus: **High Availability**

    Several Gradient servers run one instance. Builds, caches and the web interface stay up while a server is failing or updating.

-   :material-earth: **Federation**

    Gradient instances connect to each other. Builds and caches flow between instances. Other instances are never rebuilding a path that one instance already built.

</div>

</div>

</div>

## Feedback

Ideas and votes on the roadmap go to [GitHub Discussions](https://github.com/wavelens/gradient/discussions) or the Matrix room [#gradient-ci:matrix.org](https://matrix.to/#/#gradient-ci:matrix.org).
