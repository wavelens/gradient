/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ lib, ... }: with lib; let
  userType = types.submodule ({ config, ... }: {
    options = {
      username = mkOption {
        type = types.str;
        description = "Unique username for the user";
      };

      name = mkOption {
        type = types.str;
        default = config.username;
        description = "Full name of the user";
      };

      email = mkOption {
        type = types.str;
        description = "Email address of the user";
      };

      password_file = mkOption {
        type = types.str;
        description = "Path to file containing the hashed password";
      };

      email_verified = mkOption {
        type = types.bool;
        default = true;
        description = "Whether the user's email has been verified";
      };
    };
  });

  organizationType = types.submodule ({ config, ... }: {
    options = {
      name = mkOption {
        type = types.str;
        description = "Unique name for the organization";
      };

      display_name = mkOption {
        type = types.str;
        default = config.name;
        description = "Display name for the organization";
      };

      description = mkOption {
        type = types.str;
        default = "";
        description = "Description of the organization";
      };

      private_key_file = mkOption {
        type = types.str;
        description = "Path to SSH private key file for Git access";
      };

      use_nix_store = mkOption {
        type = types.bool;
        default = true;
        description = "Whether to use Nix store for this organization";
      };

      created_by = mkOption {
        type = types.str;
        description = "Username of the user who created this organization";
      };
    };
  });

  projectType = types.submodule ({ config, ... }: {
    options = {
      name = mkOption {
        type = types.str;
        description = "Unique name for the project";
      };

      organization = mkOption {
        type = types.str;
        description = "Name of the organization this project belongs to";
      };

      display_name = mkOption {
        type = types.str;
        default = config.name;
        description = "Display name for the project";
      };

      description = mkOption {
        type = types.str;
        default = "";
        description = "Description of the project";
      };

      repository = mkOption {
        type = types.str;
        description = "Git repository URL for the project";
      };

      evaluation_wildcard = mkOption {
        type = types.str;
        default = "packages.x86_64-linux.*";
        description = "Branch or pattern for evaluations";
      };

      active = mkOption {
        type = types.bool;
        default = true;
        description = "Whether the project is active";
      };

      force_evaluation = mkOption {
        type = types.bool;
        default = false;
        description = "Whether to force evaluation on next check";
      };

      created_by = mkOption {
        type = types.str;
        description = "Username of the user who created this project";
      };
    };
  });


  serverType = types.submodule ({ config, ... }: {
    options = {
      name = mkOption {
        type = types.str;
        description = "Unique name for the server";
      };

      display_name = mkOption {
        type = types.str;
        default = config.name;
        description = "Display name for the server";
      };

      organization = mkOption {
        type = types.str;
        description = "Name of the organization this server belongs to";
      };

      active = mkOption {
        type = types.bool;
        default = true;
        description = "Whether the server is active";
      };

      host = mkOption {
        type = types.str;
        default = "localhost";
        description = "Hostname or IP address of the server";
      };

      port = mkOption {
        type = types.int;
        default = 22;
        description = "SSH port of the server";
      };

      username = mkOption {
        type = types.str;
        description = "SSH username for connecting to the server";
      };

      architectures = mkOption {
        type = types.listOf (types.enum [ "x86_64-linux" "aarch64-linux" "x86_64-darwin" "aarch64-darwin" ]);
        default = [ "x86_64-linux" ];
        description = "List of architectures supported by this server";
      };

      features = mkOption {
        type = types.listOf types.str;
        default = [ ];
        description = "List of feature names supported by this server";
      };

      created_by = mkOption {
        type = types.str;
        description = "Username of the user who created this server";
      };
    };
  });

  cacheType = types.submodule ({ config, ... }: {
    options = {
      name = mkOption {
        type = types.str;
        description = "Unique name for the cache";
      };

      display_name = mkOption {
        type = types.str;
        default = config.name;
        description = "Display name for the cache";
      };

      description = mkOption {
        type = types.str;
        default = "";
        description = "Description of the cache";
      };

      active = mkOption {
        type = types.bool;
        default = true;
        description = "Whether the cache is active";
      };

      priority = mkOption {
        type = types.int;
        default = 10;
        description = "Priority of the cache (higher is more important)";
      };

      signing_key_file = mkOption {
        type = types.str;
        description = "Path to file containing the Nix cache signing key";
      };

      organizations = mkOption {
        type = types.listOf types.str;
        default = [ ];
        description = "List of organization names that can use this cache";
      };

      created_by = mkOption {
        type = types.str;
        description = "Username of the user who created this cache";
      };
    };
  });

  apiKeyType = types.submodule {
    options = {
      name = mkOption {
        type = types.str;
        description = "Name of the API key";
      };

      key_file = mkOption {
        type = types.str;
        description = "Path to file containing the API key value";
      };

      owned_by = mkOption {
        type = types.str;
        description = "Username of the user who owns this API key";
      };
    };
  };

  stateType = types.submodule {
    options = {
      users = mkOption {
        type = types.listOf userType;
        default = [ ];
        description = "List of users to create";
      };

      organizations = mkOption {
        type = types.listOf organizationType;
        default = [ ];
        description = "List of organizations to create";
      };

      projects = mkOption {
        type = types.listOf projectType;
        default = [ ];
        description = "List of projects to create";
      };

      servers = mkOption {
        type = types.listOf serverType;
        default = [ ];
        description = "List of servers to create";
      };

      caches = mkOption {
        type = types.listOf cacheType;
        default = [ ];
        description = "List of caches to create";
      };

      api_keys = mkOption {
        type = types.listOf apiKeyType;
        default = [ ];
        description = "List of API keys to create";
      };

    };
  };

in
{
  options.services.gradient = {
    state = mkOption {
      type = stateType;
      default = { };
      description = "Gradient state configuration for users, organizations, projects, servers, and caches";
      example = literalExpression ''
        {
          users = [
            {
              username = "alice";
              name = "Alice Johnson";
              email = "alice@example.com";
              password_file = "/etc/gradient/secrets/alice_password";
              email_verified = true;
            }
          ];
          organizations = [
            {
              name = "acme-corp";
              display_name = "ACME Corporation";
              description = "Main development organization";
              private_key_file = "/etc/gradient/secrets/acme_ssh_key";
              use_nix_store = true;
              created_by = "alice";
            }
          ];
          projects = [
            {
              name = "web-app";
              organization = "acme-corp";
              display_name = "Web Application";
              description = "Main web application";
              repository = "https://github.com/acme-corp/web-app.git";
              evaluation_wildcard = "main";
              active = true;
              created_by = "alice";
            }
          ];
          servers = [
            {
              name = "build-server-1";
              display_name = "Build Server 1";
              organization = "acme-corp";
              host = "build1.internal.acme.com";
              username = "gradient";
              architectures = [ "x86_64-linux" "aarch64-linux" ];
              features = [ "big-parallel" ];
              created_by = "alice";
            }
          ];
          caches = [
            {
              name = "main-cache";
              display_name = "Main Binary Cache";
              description = "Primary binary cache";
              signing_key_file = "/etc/gradient/secrets/main_cache_key";
              organizations = [ "acme-corp" ];
              created_by = "alice";
            }
          ];
        }
      '';
    };
  };
}
