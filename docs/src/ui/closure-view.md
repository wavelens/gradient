# Closure View

Where the size of a build output comes from, as a Sankey diagram of the closure. Useful for trimming ISOs, netboot images and container layers. **View Closure** on an entry point's metrics page opens the closure of the newest build.

<!-- screenshot: closure view Sankey diagram of a NixOS system -->

| Area | Shows | Actions |
|---|---|---|
| Header | Total closure size, closure type, truncation warning | Zoom in, zoom out, fit to screen |
| Diagram | Packages as bars, flowing from dependencies on the left into the root on the right | Scroll to zoom, drag to pan |

A bar's height is the package's closure size: its own NAR plus everything the package pulls in. The tallest bars are the best candidates to remove.

## Runtime and Build Closure

| Closure | Contains | Open with |
|---|---|---|
| Runtime | Store paths the outputs reference, what the output needs to run | Default |
| Build | Every derivation needed to build the output | `?type=build` in the URL |

The runtime closure covers only outputs already in the cache.

## Large Closures

- The 500 largest packages show one by one; the rest collapse into an **others** bar under the nearest shown package.
- Each package shows under one parent only, so the bars add up to the total.
- The total stays exact, even when the header warns about truncation.

## Related

- [Evaluations and Builds](../concepts/evaluations-and-builds.md): entry points and builds
- [API](../reference/api.md#examples): the closure endpoints for scripts
