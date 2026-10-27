# Roadmap

Upcoming releases with the features of each.

<div class="timeline" markdown>

<div class="timeline-item now" markdown>

## v2.1.0

Jobs beyond the Nix sandbox.

<div class="grid cards" markdown>

-   :material-play-network: **Exec Workers**

    Workers that build outside the sandbox. Integration testing with network access, real hardware or deployment credentials.

-   :material-clipboard-list: **SBOM Support**

    A CycloneDX software bill of materials for every build. License audits and vulnerability scanners get the full runtime closure.

-   :material-graph-outline: **Dynamic Derivations and Recursive Nix**

    Builds generate derivations at build time. Dynamic derivations and nested Nix invocations inside a build both count. Gradient can build and cache each generated derivation like any other.

</div>

</div>

<div class="timeline-item next" markdown>

## v2.2.0

Faster evaluation of large flakes.

<div class="grid cards" markdown>

-   :material-graph: **Multi-Node Evaluations**

    An evaluation split over many workers. Large flakes like nixpkgs or fleets of NixOS hosts finish in a fraction of the time.

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
