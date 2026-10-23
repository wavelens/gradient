<!--
SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
SPDX-License-Identifier: AGPL-3.0-only
-->

# Evaluation Wildcards

Each task lists attribute paths, separated by commas. Gradient builds the flake outputs that these paths select. By default the list contains `packages.x86_64-linux.**`.

## Examples

| Wildcard | Builds |
|---|---|
| `packages.x86_64-linux.*` | All packages on `x86_64-linux` |
| `packages.*.*` | All packages on all systems |
| `packages.x86_64-linux.*,checks.x86_64-linux.*` | Packages plus checks on `x86_64-linux` |
| `nixosConfigurations.*.config.system.build.toplevel` | All NixOS systems |
| `devShells.x86_64-linux.*` | All dev shells on `x86_64-linux` |
| `packages.x86_64-linux.*,!packages.x86_64-linux.broken` | All packages except `broken` |
| `legacyPackages.x86_64-linux.**` | All packages, nested sets like `python3Packages` included |
| `packages.x86_64-*.*` | All packages on `x86_64` systems |
| `my."python3.12".**` | Attribute names that contain dots, in quotes |
| `**` | All outputs beneath the listed roots |

## Syntax

| Element | Meaning |
|---|---|
| `a.b.c` | One attribute path, with `.` between each segment |
| `,` | Separator between patterns. A space after the comma can be omitted |
| `*` | Matches any name at one level. At the end of a pattern, only derivations on that level match |
| `**` | Matches any name at one level, plus a nested attribute set on the next level if the pattern ends there. `a.**.**` and `a.**` mean the same thing |
| `x86_64-*` | Matches part of a name at one level |
| `!path` | Removes an exact path that earlier patterns selected |
| `"a.b"` | A segment in quotes, needed for names that contain dots or other special characters |

## Levels

| Wildcard | Selection |
|---|---|
| `packages.x86_64-linux.*` | The derivations directly under `x86_64-linux` |
| `packages.x86_64-linux.**` | The same, plus derivations inside attribute sets one level deeper |

Flake outputs hold derivations at a fixed depth. A single `*` therefore suffices for flat outputs such as `packages`. Nested package sets, like `python3Packages` inside `legacyPackages`, require `**`. The default `packages.x86_64-linux.**` handles both cases.

`**` cannot descend into opaque option trees. A single-level `*` can enter every set. Patterns such as `nixosConfigurations.*.config.system.build.toplevel` can thus reach into a configuration.

## Roots

A path begins with one of these roots: `checks`, `packages`, `formatter`, `legacyPackages`, `nixosConfigurations`, `devShells`, `hydraJobs`.

## Rejected Wildcards

- Whitespace at the start or end of a pattern, whitespace within a pattern, and empty patterns such as `a,,b` or a pattern with a trailing comma.
- A pattern that begins with `.`, a lone `!`, or a lone `#`. Old wildcards that use `#` require a rewrite to `*` and `**`.
- `*`, `**`, or a part-name glob inside an exclusion. Exclusions match exact paths only.
- A `!` within a path (for example `a.!b`), or a quoted segment consisting only of `*`, `**`, `#`, or `!`.

## Related

- [Projects and Tasks](../concepts/projects-and-tasks.md): where wildcards live
- [First Project](../get-started/first-project.md): how to use a wildcard in a new task
