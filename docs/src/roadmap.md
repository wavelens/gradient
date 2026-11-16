<!--
SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
SPDX-License-Identifier: AGPL-3.0-only
-->

# Roadmap

Upcoming releases with the features of each.

<div class="timeline" markdown>

<div class="timeline-item now" markdown>

## v2.1.0

Exec jobs outside the Nix sandbox and faster evaluation of large flakes.

<div class="grid cards" markdown>

-   :material-play-network: **Exec Jobs**

    Programs in a VM on a worker, outside the Nix build sandbox.

    - Integration tests that need network access
    - Test driver for NixOS
    - Deployments with credentials
    - VM clusters across multiple workers

-   :material-graph: **Multi-Node Evaluations**

    Evaluations spread across many workers. Large flakes such as nixpkgs or a fleet of NixOS hosts finish in a fraction of the time.

-   :material-clipboard-list: **SBOM Support**

    A CycloneDX software bill of materials for every build. License audits and vulnerability scanners get the full runtime closure.

-   :material-graph-outline: **Dynamic Derivations and Recursive Nix**

    Builds generate derivations at build time. Dynamic derivations and nested Nix invocations inside a build both count. Gradient can build and cache each generated derivation like any other.

</div>

!!! warning "Upgrade"

    Servers reject workers older than protocol 65. Workers reject servers older than protocol 65. Upgrade both in one maintenance window.

</div>

<div class="timeline-item next" markdown>

## v2.2.0

Plugins extend Gradient.

<div class="grid cards" markdown>

-   :material-puzzle: **Gradient Plugins**

    Plugins written in Rust and compiled to WebAssembly. Administrators load them through the web interface. No rebuild or restart of Gradient.

</div>

</div>

<div class="timeline-item later" markdown>

## v3.0.0

Gradient without a single point of failure.

<div class="grid cards" markdown>

-   :material-server-plus: **High Availability**

    A group of Gradient servers can run an instance. Builds, caches and the web interface stay up during the failure or update of a server.

-   :material-earth: **Federation**

    Gradient instances connect to each other. Builds and caches flow between instances. Other instances rebuild no path that an instance already built.

</div>

</div>

</div>

## Feedback

Ideas and votes on the roadmap go to [GitHub Discussions](https://github.com/wavelens/gradient/discussions) or the Matrix room [#gradient-ci:matrix.org](https://matrix.to/#/#gradient-ci:matrix.org).
