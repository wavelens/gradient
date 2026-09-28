# Evaluation Wildcards

A task's wildcard selects which flake outputs Gradient builds: comma-separated attribute paths, where `*` and `#` match any name at their level. The default is `packages.x86_64-linux.*`.

## Examples

| Wildcard | Builds |
|---|---|
| `packages.x86_64-linux.#` | Every package for `x86_64-linux` |
| `packages.#.#` | Every package for every system |
| `packages.x86_64-linux.#,checks.x86_64-linux.#` | Packages and checks for `x86_64-linux` |
| `nixosConfigurations.#.config.system.build.toplevel` | Every NixOS system |
| `devShells.x86_64-linux.#` | Every dev shell for `x86_64-linux` |
| `packages.x86_64-linux.#,!packages.x86_64-linux.broken` | Every package except one |
| `legacyPackages.x86_64-linux.*` | Every package including nested package sets, e.g. `python3Packages` |
| `my."python3.12".*` | Attribute names with dots, quoted |
| `*` | Every output under the allowed roots |

## Syntax

| Element | Meaning |
|---|---|
| `a.b.c` | One attribute path, segments separated by `.` |
| `,` | Separates patterns; a space after the comma is allowed |
| `*` | Any name at this level; at the end, also one level deeper; `a.*.*` equals `a.*` |
| `#` | Any name at this level, only derivations, never deeper |
| `!path` | Removes an exact path selected by the earlier patterns |
| `"a.b"` | A quoted segment, for names with dots or special characters |

## `*` and `#`

```text
packages.x86_64-linux.#   # only the derivations directly under x86_64-linux
packages.x86_64-linux.*   # the same, plus derivations inside attribute sets one level down
```

`#` is recommended over the `*` default: flake outputs keep derivations at a fixed depth, and `#` selects exactly that depth. `*` fits outputs with nested package sets, such as `legacyPackages`.

## Roots

The first segment is one of `checks`, `packages`, `formatter`, `legacyPackages`, `nixosConfigurations`, `devShells`, `hydraJobs`.

## Rejected Wildcards

- Leading or trailing whitespace, whitespace inside a pattern, empty patterns (`a,,b`, a trailing comma).
- A pattern starting with `.`, a bare `!` or a bare `#`.
- `*` or `#` in an exclusion: exclusions are exact paths.
- `!` inside a path (`a.!b`), or a quoted segment that is only `*`, `#` or `!`.

## Related

- [Projects and Tasks](../concepts/projects-and-tasks.md): where the wildcard lives
- [First Project](../get-started/first-project.md): the wildcard in a new task
