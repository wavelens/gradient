# Roadmap

**Where Gradient goes next.** The upcoming releases and the features each one brings.

<div class="timeline" markdown>

<div class="timeline-item now" markdown>

## v2.0.0

The first release with a stability pledge.

<div class="grid cards" markdown>

-   :material-shield-check: **Stability Pledge**

    Stable, professional releases become a commitment. Each release includes migrations for the NixOS module options, the PostgreSQL schema, cache storage, the API and the worker protocol.

-   :material-office-building: **Organizations**

    A new level above projects. One organization holds the projects, members and workers of a team.

-   :material-database-sync: **Storage Migrations**

    Changes to the S3 and file layout migrate existing caches in place. An interrupted migration resumes after a restart.

-   :material-broom: **Continuous Deep GC**

    Deep garbage collection runs slowly in the background at all times. Checkpoints let each pass resume where the last one stopped.

-   :material-palette: **Corporate Design**

    Own logo, name and colors for the web interface.

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

    Workers that execute jobs outside the sandbox. Integration tests with network access, real hardware or deployment credentials.

</div>

</div>

</div>

## Feedback

Ideas and votes on the roadmap go to [GitHub Discussions](https://github.com/wavelens/gradient/discussions) or the Matrix room [#gradient-ci:matrix.org](https://matrix.to/#/#gradient-ci:matrix.org).
