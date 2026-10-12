# Evaluation Wildcards

Tasks hold a comma-separated list of attribute paths and these paths pick the flake outputs for Gradient to build. The default is `packages.x86_64-linux.**`.

## Examples

| Wildcard | Builds |
|---|---|
| `packages.x86_64-linux.*` | Every package for `x86_64-linux` |
| `packages.*.*` | Every package for every system |
| `packages.x86_64-linux.*,checks.x86_64-linux.*` | Packages and checks for `x86_64-linux` |
| `nixosConfigurations.*.config.system.build.toplevel` | Every NixOS system |
| `devShells.x86_64-linux.*` | Every dev shell for `x86_64-linux` |
| `packages.x86_64-linux.*,!packages.x86_64-linux.broken` | Every package apart from `broken` |
| `legacyPackages.x86_64-linux.**` | Every package including nested package sets, e.g. `python3Packages` |
| `packages.x86_64-*.*` | Every package for the `x86_64` systems |
| `my."python3.12".**` | Attribute names with dots, quoted |
| `**` | Every output under the listed roots |

## Syntax

| Element | Meaning |
|---|---|
| `a.b.c` | One attribute path, segments separated by `.` |
| `,` | Pattern separator, with an optional space after the comma |
| `*` | Any name at this level. At the end of a pattern, only derivations at that level |
| `**` | Any name at this level and a nested attribute set one level below when the pattern ends there. `a.**.**` is equal to `a.**` |
| `x86_64-*` | Part of a name at this level |
| `!path` | Removal of an exact path selected by the earlier patterns |
| `"a.b"` | A quoted segment, for names with dots or other special characters |

## Levels

| Wildcard | Selection |
|---|---|
| `packages.x86_64-linux.*` | The derivations directly under `x86_64-linux` |
| `packages.x86_64-linux.**` | The same, with the derivations inside attribute sets one level down |

Flake outputs keep derivations at a fixed depth, so `*` suits an output such as `packages`. A nested package set, such as `python3Packages` inside `legacyPackages`, needs `**`. The default `packages.x86_64-linux.**` suits both.

Option trees stay opaque to a wildcard.

## Roots

The first segment is one of `checks`, `packages`, `formatter`, `legacyPackages`, `nixosConfigurations`, `devShells`, `hydraJobs`.

## Rejected Wildcards

- Leading or trailing whitespace, whitespace inside a pattern, empty patterns (`a,,b`, a trailing comma).
- A pattern starting with `.`, a bare `!`, or a bare `#`. Older wildcards with `#` need a rewrite to `*` and `**`.
- `*`, `**` or a part-name glob in an exclusion. Exclusions are exact paths.
- `!` inside a path (`a.!b`), or a quoted segment that is only `*`, `**`, `#` or `!`.

## Related

- [Projects and Tasks](../concepts/projects-and-tasks.md): home of the wildcard
- [First Project](../get-started/first-project.md): the wildcard in a new task
