# Roadmap

Upcoming releases and their features.

<div class="timeline" markdown>

<div class="timeline-item now" markdown>

## v2.0.0

The first release with a stability pledge.

<div class="grid cards" markdown>

-   :material-shield-check: **Stability Pledge**

    Stable, professional releases become a commitment. Every release will include migrations for the NixOS module options, the PostgreSQL schema, cache storage, the API and the worker protocol.

-   :material-database-sync: **Storage Migrations**

    Changes to the S3 and file layout migrate existing caches in place. An interrupted migration can resume after a restart.

-   :material-broom: **Continuous Deep GC**

    Deep garbage collection will run slowly in the background at all times. Checkpoints let each pass resume where the last one stopped.

-   :material-server-network: **Cluster Jobs (beta)**

    Jobs that run on many workers at once. Gradient will allocate all members together. Jobs needing a fast interconnect get all members from one [zone](concepts/workers.md#zones).

-   :material-console-network: **SSH Builds**

    An [ssh-ng:// store](guides/build-over-ssh.md) for every project. `nixos-rebuild --build-host` and `nix copy` talk straight to the CI workers and caches.

-   :material-palette: **Gradient.CI Servers**

    Gradient Remote Worker Pay-As-You-Go Service from [Gradient.CI Servers](https://servers.gradient.ci). This Service will help use to continue the development of Gradient.

-   :material-palette: **Gradient Teams**

    Organizational structures for better team management with SSO.

-   :material-palette: **Corporate Design**

    Own logo for web interface.

</div>

</div>

<div class="timeline-item next" markdown>

## v2.1.0

Jobs beyond the Nix sandbox.

<div class="grid cards" markdown>

-   :material-play-network: **Runner Workers**

    Workers that build outside the sandbox. Integration testing with network access, real hardware or deployment credentials.

-   :material-play-network: **Input-From-Derivation Support**

    Gradient gets full IFD Support.

</div>

</div>

<div class="timeline-item later" markdown>

## v2.2.0

Faster evaluation of large flakes.

<div class="grid cards" markdown>

-   :material-graph: **Multi-Node Evaluations**

    One evaluation split across many workers. Large flakes like nixpkgs or fleets of NixOS hosts finish in a fraction of the time.

</div>

</div>

<div class="timeline-item later" markdown>

## v3.0.0

Gradient without a single point of failure.

<div class="grid cards" markdown>

-   :material-server-plus: **High Availability**

    A group of Gradient servers can run one instance. Builds, caches and the web interface stay up during the failure or update of a server.

-   :material-earth: **Federation**

    Gradient instances connect to each other. Builds and caches flow between instances. Other instances will never rebuild a path that one instance already built.

</div>

</div>

</div>

## Feedback

Ideas and votes on the roadmap go to [GitHub Discussions](https://github.com/wavelens/gradient/discussions) or the Matrix room [#gradient-ci:matrix.org](https://matrix.to/#/#gradient-ci:matrix.org).
