<!--
SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
SPDX-License-Identifier: AGPL-3.0-only
-->

# Evaluation Wildcards

A task's wildcard will select the flake outputs for Gradient to build. The wildcard is a comma-separated list of attribute paths. `*` and `#` are matching any name at their level. The default is `packages.x86_64-linux.*`.

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
| `,` | Pattern separator. A space after the comma is allowed |
| `*` | Any name at this level. A trailing `*` is also matching one level deeper. `a.*.*` is equal to `a.*` |
| `#` | Any name at this level, only derivations, never deeper |
| `!path` | Removal of an exact path selected by the earlier patterns |
| `"a.b"` | A quoted segment, for names with dots or special characters |

## `*` and `#`

```text
packages.x86_64-linux.#   # only the derivations directly under x86_64-linux
packages.x86_64-linux.*   # the same, plus derivations inside attribute sets one level down
```

`#` is the recommended choice over the `*` default. Flake outputs are keeping derivations at a fixed depth, and `#` will select exactly that depth. A `*` is the right fit for outputs with nested package sets, such as `legacyPackages`.

## Roots

The first segment is one of `checks`, `packages`, `formatter`, `legacyPackages`, `nixosConfigurations`, `devShells`, `hydraJobs`.

## Rejected Wildcards

- Leading or trailing whitespace, whitespace inside a pattern, empty patterns (`a,,b`, a trailing comma).
- A pattern starting with `.`, a bare `!` or a bare `#`.
- `*` or `#` in an exclusion. Exclusions are exact paths.
- `!` inside a path (`a.!b`), or a quoted segment that is only `*`, `#` or `!`.

## Related

- [Projects and Tasks](../concepts/projects-and-tasks.md): home of the wildcard
- [First Project](../get-started/first-project.md): the wildcard in a new task
