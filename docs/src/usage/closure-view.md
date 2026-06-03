# Closure View

The closure view shows a Sankey diagram of a build's (or evaluation's) full
dependency closure, sized by uncompressed NAR size. It exists to answer "what is
taking up space?" when trimming netboot images, ISOs, or container layers.

Open it from an entry point's metrics page via the **View Closure** button, which
links to the closure of that entry point's most recent build.

The diagram renders the largest packages individually and aggregates the long
tail into a single **others** node; the header shows the exact total closure
size and warns when a very large closure had its node list truncated (the total
stays exact regardless).

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
