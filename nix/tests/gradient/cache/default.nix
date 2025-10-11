/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ pkgs, ... }: {
  value = pkgs.testers.runNixOSTest ({ pkgs, lib, ... }: {
    name = "gradient-cache";
    globalTimeout = 480;

    defaults = {
      networking.firewall.enable = false;
      virtualisation.writableStore = true;
      documentation.enable = false;
      nix.settings.substituters = lib.mkForce [ ];
    };

    nodes = {
      server = { config, pkgs, lib, ... }: {
        imports = [
          ../../../modules/gradient.nix
        ];

        systemd.services.gradient-server.environment.GRADIENT_DEBUG = lib.mkForce "true";
        environment.etc = {
          "gradient/secrets/admin_password" = {
            mode = "0600";
            user = "gradient";
            group = "gradient";
            text = "admin_password";
          };

          "gradient/secrets/main_cache_key" = {
            mode = "0600";
            user = "gradient";
            group = "gradient";
            text = "cache-priv-key:AQN7Q0NCAgAAADIAGnOTVu8LQJdawFtL/3SBUo5OBrXo7tZHgH4LbAEwZNKZUBHv5MQAAABAFQYMSsB=";
          };
        };

        networking.hosts = {
          "127.0.0.1" = [ "gradient.local" "oidc.local" ];
        };

        services = {
          gradient = {
            enable = true;
            serveCache = true;
            configureNginx = true;
            configurePostgres = true;
            domain = "gradient.local";
            jwtSecretFile = toString (pkgs.writeText "jwtSecret" "b68a8eaa8ebcff23ebaba1bd74ecb8a2eb7ba959570ff8842f148207524c7b8d731d7a1998584105e951599221f9dcd20e41223be17275ca70ab6f7e6ecafa8d4f8905623866edb2b344bd15de52ccece395b3546e2f00644eb2679cf7bdaa156fd75cc5f47c34448cba19d903e68015b1ad3c8e9d04862de0a2c525b6676779012919fa9551c4746f9323ab207aedae86c28ada67c901cae821eef97b69ca4ebe1260de31add34d8265f17d9c547e3bbabe284d9cadcc22063ee625b104592403368090642a41967f8ada5791cb09703d0762a3175d0fe06ec37822e9e41d0a623a6349901749673735fdb94f2c268ac08a24216efb058feced6e785f34185a");
            cryptSecretFile = toString (pkgs.writeText "cryptSecret" "aW52YWxpZC1pbnZhbGlkLWludmFsaWQK");
            state = {
              users = [{
                username = "admin";
                email = "admin@example.com";
                password_file = "/etc/gradient/secrets/admin_password";
              }];

              organizations = [{
                name = "org";
                private_key_file = "/etc/gradient/secrets/acme_ssh_key";
                created_by = "admin";
              }];

              projects = [{
                name = "project";
                organization = "org";
                repository = "";
                created_by = "admin";
              }];

              servers = [{
                name = "build-server-1";
                display_name = "Build Server 1";
                organization = "acme-corp";
                active = true;
                host = "build1.internal.acme.com";
                port = 22;
                username = "gradient";
                architectures = [ "x86_64-linux" "aarch64-linux" ];
                features = [ "big-parallel" ];
                created_by = "admin";
              }];

              caches = [{
                name = "main";
                signing_key_file = "/etc/gradient/secrets/main_cache_key";
                organizations = [ "org" ];
                created_by = "admin";
              }];
            };
          };

          postgresql = {
            package = pkgs.postgresql_18;
            enableTCPIP = true;
            authentication = ''
              #...
              #type database DBuser origin-address auth-method
              # ipv4
              host  all      all     0.0.0.0/0      trust
              # ipv6
              host all       all     ::0/0        trust
            '';

            settings = {
              log_connections = true;
              logging_collector = true;
              log_disconnections = true;
              log_destination = lib.mkForce "syslog";
            };
          };
        };

        nix.settings = {
          max-jobs = 0;
        };
      };
    };

    interactive.nodes = {
      server = import ../../modules/debug-host.nix;
    };

    testScript = { nodes, ... }:
      ''
      start_all()

      server.wait_for_unit("gradient-server.service")
      server.sleep(5)
      '';
  });
}
