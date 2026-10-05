<h1 align="center">Gradient</h1>

<p align="center"><b>Nix-CI for Teams.</b> Every flake built once, on any machine, cached for everyone.</p>

<p align="center">
  <a href="https://wavelens.github.io/gradient/get-started/quick-start/">🚀 Quick Start</a>
  •
  <a href="https://wavelens.github.io/gradient">📖 Documentation</a>
  •
  <a href="https://public.gradient.ci">🌐 Public Instance</a>
  •
  <a href="https://wavelens.github.io/gradient-search">🔍 Options Search</a>
  •
  <a href="https://matrix.to/#/#gradient-ci:matrix.org">💬 Matrix Chat</a>
  •
  <sup><a href="https://public.gradient.ci/project/gradient/task/main">
    <img src="https://public.gradient.ci/api/v1/tasks/gradient/main/badge" alt="Gradient Badge" align="middle">
  </a></sup>
</p>

![Gradient](./docs/src/assets/screenshots/gradient.png)

<p align="center"><a href="./docs/gallery.md">📸 Screenshot Gallery</a></p>


https://github.com/user-attachments/assets/bd733d97-fd8b-4439-b880-04387ba1e073

Gradient evaluates and builds Nix flakes on a pool of workers, starts builds while the evaluation is still running and serves every result from a built-in binary cache.

## Features

| | |
|---|---|
| **[Git Host Integration](https://wavelens.github.io/gradient/guides/github/)** | GitHub, Gitea / Forgejo and GitLab: builds on push and pull request. Sends status checks back |
| **[Build Before Pushing](https://wavelens.github.io/gradient/guides/build-before-push/)** | Uncommitted changes built on the CI workers with `gradient build` |
| **[Built-in Binary Cache](https://wavelens.github.io/gradient/concepts/caches/)** | Per-project caches with S3 storage, signing and sharing between projects |
| **[Scales With Workers](https://wavelens.github.io/gradient/concepts/workers/)** | Evaluation and builds both run on workers; each added machine adds capacity |
| **[MCP Server](https://wavelens.github.io/gradient/guides/mcp/)** | Failed builds, logs and evaluations, readable by any AI assistant |
| **[Pull Deployment](https://wavelens.github.io/gradient/guides/pull-deployment/)** | Machines fetch and switch to their latest built NixOS configuration on their own |
| **[Declarative Setup](https://wavelens.github.io/gradient/reference/state/)** | Users, projects, caches and workers as NixOS options, validated at build time |
| **[SSO and Teams](https://wavelens.github.io/gradient/guides/sso/)** | OIDC login, SCIM provisioning, roles and invites per project and cache |

## CLI

[Download Gradient CLI](https://public.gradient.ci/api/v1/tasks/gradient/main/entry-point-downloads?eval=packages.x86_64-linux.gradient-cli-static&filename=gradient): a static Linux binary, no Nix needed. Build uncommitted changes on the CI workers:

```sh
# Install Gradient
curl -fLo gradient "https://public.gradient.ci/api/v1/tasks/gradient/main/entry-point-downloads?eval=packages.x86_64-linux.gradient-cli-static&filename=gradient"
chmod +x gradient && sudo mv gradient /usr/local/bin/
# or via Nix: nix run github:wavelens/gradient/latest#gradient-cli-full -- [flags]

gradient login https://gradient.example.com
gradient build .#hello
```

All commands: [CLI Reference](https://wavelens.github.io/gradient/reference/cli/).

## Compared to Hydra and GitHub Actions

| | [Hydra](https://github.com/NixOS/hydra) | GitHub Actions + [Cachix](https://www.cachix.org) | Gradient |
|---|---|---|---|
| Build start | After the whole evaluation finishes | After the job has evaluated the flake | While the evaluation is still running |
| Evaluation | On the server, limited by one machine | Inside each job, limited by the runner | On workers, scales with them |
| Nix store | Kept on the server and builders | Empty on every job; each job downloads its closure again | Kept on the workers between builds |
| Shared work | One build per derivation | Parallel jobs can build the same derivation twice | Every derivation built once, shared across projects |
| Build outputs | Pass through the server | Pushed from the runner to Cachix | Large outputs go from worker straight to S3 |
| Server host | Needs a writable Nix store | Hosted by GitHub | Needs no Nix store, fits in a micro-VM |
| Heavy builds | Static machine list with speed factors | Fixed runner sizes | Scoring system places them by predicted memory, learned from past builds |
| Private caches | One store for the whole instance | Free plan: 5 GB, filled quickly by full closures | Per-project caches with access control, on own S3 or disk storage |
| Sign-in | Local accounts, LDAP, OIDC with roles for the whole instance | GitHub accounts | [OIDC](https://wavelens.github.io/gradient/guides/sso/) with provider groups mapped to roles per project, SCIM provisioning |
| Integrations | Minimal JSON API | GitHub only | REST API, webhooks, Git Integrations and MCP server |
| Web UI | Server-rendered pages | GitHub job logs | Responsive UI with live log streaming |
| Stars | ![](https://img.shields.io/github/stars/NixOS/hydra?style=for-the-badge&labelColor=rgba(225%2C227%2C232%2C0.82)&color=rgba(30%2C34%2C42%2C1)&label=high) | - | ![](https://img.shields.io/github/stars/wavelens/gradient?style=for-the-badge&labelColor=rgba(225%2C227%2C232%2C0.82)&color=rgba(30%2C34%2C42%2C1)&label=low) |

## Installation

A NixOS module sets up the server, a local worker, PostgreSQL and the reverse proxy. The [Quick Start](https://wavelens.github.io/gradient/get-started/quick-start/) walks through the setup in three steps.

For a first try on a personal repository, the [Standalone](https://wavelens.github.io/gradient/get-started/standalone/) instance is running everything in one container or VM:

```sh
docker run -dt --name gradient --privileged --cgroupns=host -p 127.0.0.1:8080:80 -v gradient:/var/lib ghcr.io/wavelens/gradient-standalone
# or
nix run github:wavelens/gradient/latest#standalone
```

Pre-built Gradient packages:

```text
URL:        https://public.gradient.ci/cache/main
Public Key: public.gradient.ci-main:qmxRE+saUvhNa3jqaCMWje+feVU77TjABchZrPGf7A8=
```

## API

A REST API backs the web UI and the CLI: [OpenAPI spec](./docs/gradient-api.yaml), [Swagger UI](https://petstore.swagger.io/?url=https://raw.githubusercontent.com/wavelens/gradient/main/docs/gradient-api.yaml).

## Roadmap

Planned releases in the [Roadmap](https://wavelens.github.io/gradient/roadmap/). Feedback in [GitHub Discussions](https://github.com/wavelens/gradient/discussions) or on [Matrix](https://matrix.to/#/#gradient-ci:matrix.org).

## Contributing

Contributions are welcome: see the [Contributing Guidelines](CONTRIBUTING.md) and the [Contributor Docs](https://wavelens.github.io/gradient/contributors/).

## License

[AGPL-3.0-only](./LICENSE), with license notices following the [REUSE guidelines](https://reuse.software/). Developed by [Wavelens GmbH](https://wavelens.io).
