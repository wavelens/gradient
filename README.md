# Gradient

[Options Search](https://wavelens.github.io/gradient-search)

Gradient is a web-based Nix-based Continuous Integration (CI) system.

> [!IMPORTANT]
> This project is currently in the early stages of development. We are working on the initial implementation and documentation. If you are interested in contributing, please read the [Contributing Guidelines](CONTRIBUTING.md) for more information.

## Features

![Gradient](./docs/gradient.png)

- **Modern UI**: has a clean and intuitive user interface. (planned)
- **Organizations**: multiple organizations, which work independently from each other (e.g. different servers, user access).
- **API**: provides a RESTful API with API-Key management for authentication.
- **Streaming Logs**: real-time log streaming for builds.
- **Rich Project Configuration**: check all branches, pull requests, and tags. (planned)
- **OAuth2**: support for OAuth2 for user authentication.

## Installation

Extend your `flake.nix` with Gradient module:

```nix
{
  inputs.gradient.url = "github:wavelens/gradient";
  # optional, not necessary for the module
  # inputs.gradient.inputs.nixpkgs.follows = "nixpkgs";
  # inputs.gradient.inputs.flake-utils.follows = "flake-utils";

  outputs = { self, nixpkgs, gradient, ... }: {
    # change `yourhostname` to your actual hostname
    nixosConfigurations.yourhostname = nixpkgs.lib.nixosSystem {
      # customize to your system
      system = "x86_64-linux";
      modules = [
        ./configuration.nix
        gradient.nixosModules.default
      ];
    };
  };
}
```

Configure Gradient in your `configuration.nix`:

> [!NOTE]
> All configuration options here: [Options Search](https://wavelens.github.io/gradient-search)

```nix
{
  services.gradient = {
    enable = true;
    frontend.enable = true;
    configurePostgres = true;
    configureNginx = true;
    domain = "gradient.wavelens.io";

    # we recommend the use of sops-nix
    jwtSecretFile = "/var/lib/gradient/jwt-secret";
    cryptSecretFile = "/var/lib/gradient/crypt-secret";
  };
}
```

## Usage
Gradient can be used via the web interface, API, and CLI.

### API

The API is a RESTful API that can be used to interact with Gradient programmatically.
OpenAPI documentation is available at `/docs/gradient-api.yaml`. [API Specification](./docs/gradient-api.yaml)

### Web Interface

> [!NOTE]
> The web interface is currently in development.

The web interface is the primary way to interact with Gradient. It also just uses the API.

### CLI

The CLI is also based on the API and can be used to interact with Gradient from the command line.

Install the CLI:

```nix
{
  inputs.gradient.url = "github:wavelens/gradient";
  # optional, not necessary for the module
  # inputs.gradient.inputs.nixpkgs.follows = "nixpkgs";
  # inputs.gradient.inputs.flake-utils.follows = "flake-utils";

  outputs = { self, nixpkgs, gradient, ... }: {
    # change `yourhostname` to your actual hostname
    nixosConfigurations.yourhostname = nixpkgs.lib.nixosSystem {
      # customize to your system
      system = "x86_64-linux";
      modules = [
        ./configuration.nix
        gradient.packages.${system}.gradient-cli
      ];
    };
  };
}
```

or

```sh
nix run github:wavelens/gradient#gradient-cli
```


## Contributing

We welcome contributions to this project. Please read the [Contributing Guidelines](CONTRIBUTING.md) for more information.

## License

This project is under the **GNU Affero General Public License v3.0** (AGPL-3.0; as published by the Free Software Foundation):

The [GNU Affero General Public License v3.0 (AGPL-3.0)](./LICENSE) is a free software license that ensures your freedom to use, modify, and distribute the software, with the condition that any modified versions of the software must also be distributed under the same license.

The license notice follows the [REUSE guidelines](https://reuse.software/) to ensure clarity and consistency.

## Copyright

Copyright (c) 2025, Wavelens UG
