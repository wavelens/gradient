# Closure View

The closure view shows a Sankey diagram of a build's (or evaluation's) full
dependency closure, sized by uncompressed NAR size. It exists to answer "what is
taking up space?" when trimming netboot images, ISOs, or container layers.

Open it from an entry point's metrics page via the **View Closure** button, which
links to the closure of that entry point's most recent build.

The dependency graph is a DAG, which has no conserved flow; rendering it as a
Sankey directly produces meaningless bar heights and backward links. The view
therefore reduces it to a rooted tree (each package keeps a single parent, via
breadth-first walk from the roots) and sizes every flow by the package's
**accumulated closure size** — its own NAR size plus everything it pulls in.
Flow accumulates from leaf packages on the left into the root on the right, so a
bar's height reflects how much that package contributes to the total.

The largest packages render individually; everything outside the top 500 by
closure size collapses into a per-parent **others** bucket attached to its
nearest kept ancestor. The header shows the exact total closure size and warns
when a very large closure had its node list truncated server-side (the total
stays exact regardless). The diagram uses the open-source `d3-sankey` renderer
and shares the dependency graph's controls: scroll to zoom, drag to pan, and the
zoom-in / zoom-out / fit-to-screen buttons in the header.

## API

Both endpoints return per-node sizes plus an exact `total_size_bytes`, so they
are also useful for custom tooling and scripts:

```sh
GET /api/v1/builds/{build}/closure
GET /api/v1/evals/{evaluation}/closure
```

Response (`ClosureGraph`): `roots`, `total_size_bytes`, `node_count`,
`edge_count`, `truncated`, `nodes` (`id`, `name`, `path`, `nar_size`), and
`edges` (`source` → `target`, where `target` depends on `source`).
