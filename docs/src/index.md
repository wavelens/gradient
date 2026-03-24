# Gradient

**Gradient** is a web-based, Nix-native Continuous Integration system developed by [Wavelens GmbH](https://wavelens.io).

!!! note
    This project is in active development. APIs and configuration options may change between releases.

## Features

- **Modern UI** — clean, responsive web interface built with Angular
- **Organizations** — isolated organizations with independent servers and user access
- **REST API** — full API with API-key and JWT authentication
- **Streaming Logs** — real-time log streaming for running builds
- **OAuth2 / OIDC** — integrated single-sign-on support
- **Binary Cache** — built-in Nix store cache served over HTTP
- **Remote Builds** — build Nix derivations on remote machines without a local Nix install
- **Pull Deployment** — deploy NixOS configurations by pulling from the Gradient server
- **Dependency Graph** — interactive visualization of Nix build dependency trees

## Quick Links

| Resource | Link |
|---|---|
| Source code | <https://github.com/wavelens/gradient> |
| Demo | <https://gradient.wavelens.io/api/v1/health> |
| API Reference (Swagger) | [View on Swagger UI](https://petstore.swagger.io/?url=https://raw.githubusercontent.com/wavelens/gradient/master/docs/gradient-api.yaml) |
| NixOS Options Search | <https://wavelens.github.io/gradient-search> |

## Binary Cache

A public binary cache with pre-built Gradient packages is available:

```
URL:        https://gradient.wavelens.io/cache/main
Public Key: gradient.wavelens.io-main:qmxRE+saUvhNa3jqaCMWje+feVU77TjABchZrPGf7A8=
```

## License

Gradient is released under the [GNU Affero General Public License v3.0 (AGPL-3.0)](https://github.com/wavelens/gradient/blob/main/LICENSE).
