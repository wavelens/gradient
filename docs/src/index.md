# Gradient

**Nix-CI for Teams.** Every flake built once, on any machine, cached for everyone.

![The Gradient web interface](assets/screenshots/gradient.png)

## Features

<div class="grid cards" markdown>

-   :material-source-branch: **[Forge Integration](guides/forge-github.md)**

    GitHub, Gitea / Forgejo and GitLab: builds on push and pull request. Sends status checks back.

-   :material-console: **[Build Before Pushing](guides/build-before-push.md)**

    Uncommitted changes built on the CI workers with `gradient build`, no local Nix needed.

-   :material-database: **[Built-in Binary Cache](concepts/caches.md)**

    Per-project caches with S3 storage, signing and sharing between projects.

-   :material-server-network: **[Scales With Workers](concepts/workers.md)**

    Evaluation and builds both run on workers. Each added machine adds capacity.

-   :material-robot: **[MCP Server](guides/mcp.md)**

    Failed builds, logs and evaluations, readable by any AI assistant.

-   :material-rocket-launch: **[Pull Deployment](guides/pull-deployment.md)**

    Machines fetch and switch to their latest built NixOS configuration on their own.

-   :material-file-code: **[Declarative Setup](reference/state.md)**

    Users, projects, caches and workers as NixOS options, validated at build time.

-   :material-account-group: **[SSO and Teams](guides/sso.md)**

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
| Integrations | Minimal JSON API | REST API, webhooks, Git Integrations and MCP server |
| Web UI | Server-rendered pages | Responsive UI with live log streaming |
| Stars | ![](https://img.shields.io/github/stars/NixOS/hydra?style=for-the-badge&labelColor=rgba(225%2C227%2C232%2C0.82)&color=rgba(30%2C34%2C42%2C1)&label=high) | ![](https://img.shields.io/github/stars/wavelens/gradient?style=for-the-badge&labelColor=rgba(225%2C227%2C232%2C0.82)&color=rgba(30%2C34%2C42%2C1)&label=low) |

## Public Binary Cache

Pre-built Gradient packages:

```text
URL:        https://public.gradient.ci/cache/main
Public Key: public.gradient.ci-main:qmxRE+saUvhNa3jqaCMWje+feVU77TjABchZrPGf7A8=
```

## Links

- Source code: <https://github.com/wavelens/gradient>
- API reference: [Swagger UI](https://petstore.swagger.io/?url=https://raw.githubusercontent.com/wavelens/gradient/main/docs/gradient-api.yaml)
- NixOS options search: <https://wavelens.github.io/gradient-search>
- Chat: [#gradient-ci:matrix.org](https://matrix.to/#/#gradient-ci:matrix.org)

[Get started](get-started/quick-start.md){ .md-button .md-button--primary }
[Try the public instance](https://public.gradient.ci){ .md-button }

Gradient is developed by [Wavelens GmbH](https://wavelens.io) and released under the [AGPL-3.0-only](https://github.com/wavelens/gradient/blob/main/LICENSE) license.
