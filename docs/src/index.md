# Gradient

**Nix CI for teams.** Every flake built once, on any machine, cached for everyone.

![The Gradient web interface](assets/screenshots/gradient.png)

## Features

<div class="grid cards" markdown>

-   :material-source-branch: **[Forge integration](guides/forge-github.md)**

    GitHub, Gitea / Forgejo and GitLab: builds on push and pull request. Sends status checks back.

-   :material-console: **[Build before you push](guides/build-before-push.md)**

    Uncommitted changes built on the CI workers with `gradient build`, no local Nix needed.

-   :material-database: **[Built-in binary cache](concepts/caches.md)**

    Per-project caches with S3 storage, signing and sharing between projects.

-   :material-server-network: **[Scales with workers](concepts/workers.md)**

    Evaluation and builds both run on workers. Each added machine adds capacity.

-   :material-robot: **[MCP server](guides/mcp.md)**

    Failed builds, logs and evaluations, readable by any AI assistant.

-   :material-rocket-launch: **[Pull deployment](guides/pull-deployment.md)**

    Machines fetch and switch to their latest built NixOS configuration on their own.

-   :material-file-code: **[Declarative setup](reference/state.md)**

    Users, projects, caches and workers as NixOS options, validated at build time.

-   :material-account-group: **[SSO and teams](guides/sso.md)**

    OIDC login, SCIM provisioning, roles and invites per project and cache.

</div>

## Compared to Hydra

| | [Hydra](https://github.com/NixOS/hydra) | Gradient |
|---|---|---|
| Build start | After the whole evaluation finishes | While the evaluation is still running |
| Evaluation | On the server, limited by one machine | On workers, scales with them |
| Build outputs | Pass through the server | Large outputs go from worker straight to S3 |
| Server host | Needs a writable Nix store | Needs no Nix store, fits in a micro-VM |
| Heavy builds | Static machine list with speed factors | Scoring system places them by predicted memory, learned from past builds |
| Private caches | One store for the whole instance | Per-project caches with access control |
| Sign-in | Local accounts, LDAP | OIDC single sign-on, SCIM provisioning |
| Integrations | Minimal JSON API | REST API, webhooks and MCP server |
| Web UI | Server-rendered pages | Responsive UI with live log streaming |

## Public Binary Cache

Pre-built Gradient packages:

```text
URL:        https://public.gradient.ci/cache/main
Public Key: public.gradient.ci-main:qmxRE+saUvhNa3jqaCMWje+feVU77TjABchZrPGf7A8=
```

## Links

- Source code: <https://github.com/wavelens/gradient>
- API reference: [Swagger UI](https://petstore.swagger.io/?url=https://raw.githubusercontent.com/wavelens/gradient/master/docs/gradient-api.yaml)
- NixOS options search: <https://wavelens.github.io/gradient-search>
- Chat: [#gradient-ci:matrix.org](https://matrix.to/#/#gradient-ci:matrix.org)

!!! note
    Gradient is in active development. APIs and configuration options may change between releases.

[Get started](get-started/quick-start.md){ .md-button .md-button--primary }
[Try the public instance](https://public.gradient.ci){ .md-button }

Gradient is developed by [Wavelens GmbH](https://wavelens.io) and released under the [AGPL-3.0-only](https://github.com/wavelens/gradient/blob/main/LICENSE) license.
