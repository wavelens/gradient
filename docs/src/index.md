# Gradient

**Gradient** is a web-based, Nix-native Continuous Integration system developed by [Wavelens GmbH](https://wavelens.io).

!!! note
    This project is in active development. APIs and configuration options may change between releases.

## Features

- **Modern UI**: clean and intuitive user interface
- **Organizations**: multiple organizations, which work independently from each other (e.g. different workers, user access)
- **API**: provides a RESTful API with API-Key management for authentication
- **Streaming Logs**: real-time log streaming for builds
- **Rich Project Configuration**: flake updates, check all branches, pull requests, and tags
- **OAuth2 / OIDC**: integrated single-sign-on support
- **Binary Cache**: built-in Nix store cache with S3 storage backend support
- **Proto Workers**: build and evaluate Nix derivations on distributed `gradient-worker` instances over a persistent WebSocket protocol
- **Deployment Module**: Pull-Deployment via gradient-deploy module
- **Dependency Graph**: interactive visualization of Nix build dependency trees
- **Actions Integration**: GitHub App, Gitea and Gitlab Integration

## Quick Links

| Resource | Link |
|---|---|
| Source code | <https://github.com/wavelens/gradient> |
| Demo | <https://public.gradient.ci> |
| API Reference (Swagger) | [View on Swagger UI](https://petstore.swagger.io/?url=https://raw.githubusercontent.com/wavelens/gradient/master/docs/gradient-api.yaml) |
| NixOS Options Search | <https://wavelens.github.io/gradient-search> |

## Binary Cache

A public binary cache with pre-built Gradient packages is available:

```text
URL:        https://public.gradient.ci/cache/main
Public Key: public.gradient.ci-main:qmxRE+saUvhNa3jqaCMWje+feVU77TjABchZrPGf7A8=
```

## License

Gradient is released under the [GNU Affero General Public License v3.0 (AGPL-3.0-only)](https://github.com/wavelens/gradient/blob/main/LICENSE).
