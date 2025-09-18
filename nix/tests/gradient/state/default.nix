/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ pkgs, ... }:
{
  value = pkgs.testers.runNixOSTest ({ pkgs, lib, ... }: {
    name = "gradient-state";
    globalTimeout = 120;
    nodes = {
      machine = { config, pkgs, lib, ... }: {
        imports = [ ../../../modules/gradient.nix ];
        environment.etc = {
          "gradient/secrets/alice_password" = {
            mode = "0600";
            user = "gradient";
            group = "gradient";
            text = "alice_password";
          };

          "gradient/secrets/bob_password" = {
            mode = "0600";
            user = "gradient";
            group = "gradient";
            text = "bob_password";
          };

          "gradient/secrets/acme_ssh_key" = {
            mode = "0600";
            user = "gradient";
            group = "gradient";
            text = ''
              -----BEGIN OPENSSH PRIVATE KEY-----
              b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAAAFwAAAAdzc2gtcn
              NhAAAAAwEAAQAAAQEAtest_private_key_content_here
              -----END OPENSSH PRIVATE KEY-----
            '';
          };

          "gradient/secrets/main_cache_key" = {
            mode = "0600";
            user = "gradient";
            group = "gradient";
            text = "cache-priv-key:AQN7Q0NCAgAAADIAGnOTVu8LQJdawFtL/3SBUo5OBrXo7tZHgH4LbAEwZNKZUBHv5MQAAABAFQYMSsB=";
          };

          "gradient/secrets/dev_cache_key" = {
            mode = "0600";
            user = "gradient";
            group = "gradient";
            text = "cache-priv-key:BQN7Q0NCAgAAADIAGnOTVu8LQJdawFtL/3SBUo5OBrXo7tZHgH4LbAEwZNKZUBHv5MQAAABAFQYMSsB=";
          };

          "gradient/secrets/alice_api_key" = {
            mode = "0600";
            user = "gradient";
            group = "gradient";
            text = "a1b2c3d4e5f6g7h8i9j0k1l2m3n4o5p6q7r8s9t0u1v2w3x4y5z6A7B8C9D0E1F2G3";
          };
        };

        services = {
          gradient = {
            enable = true;
            serveCache = true;
            configureNginx = true;
            configurePostgres = true;
            domain = "gradient.local";
            jwtSecretFile = toString (pkgs.writeText "jwtSecret" "b68a8eaa8ebcff23ebaba1bd74ecb8a2eb7ba959570ff8842f148207524c7b8d731d7a1998584105e951599221f9dcd20e41223be17275ca70ab6f7e6ecafa8d4f8905623866edb2b344bd15de52ccece395b3546e2f00644eb2679cf7bdaa156fd75cc5f47c34448cba19d903e68015b1ad3c8e9d04862de0a2c525b6676779012919fa9551c4746f9323ab207aedae86c28ada67c901cae821eef97b69ca4ebe1260de31add34d8265f17d9c547e3bbabe284d9cadcc22063ee625b104592403368090642a41967f8ada5791cb09703d0762a3175d0fe06ec37822e9e41d0a623a6349901749673735fdb94f2c268ac08a24216efb058feced6e785f34185a");
            cryptSecretFile = toString (pkgs.writeText "cryptSecret" "aW52YWxpZAo=");
            settings.logLevel = "info";
            state = {
              users = [
                {
                  username = "alice";
                  name = "Alice Johnson";
                  email = "alice@example.com";
                  password_file = "/etc/gradient/secrets/alice_password";
                  email_verified = true;
                }
                {
                  username = "bob";
                  name = "Bob Smith";
                  email = "bob@example.com";
                  password_file = "/etc/gradient/secrets/bob_password";
                  email_verified = false;
                }
              ];

              organizations = [{
                name = "acme-corp";
                display_name = "ACME Corporation";
                description = "Main development organization for ACME products";
                private_key_file = "/etc/gradient/secrets/acme_ssh_key";
                use_nix_store = true;
                created_by = "alice";
              }];

              projects = [
                {
                  name = "web-app";
                  organization = "acme-corp";
                  display_name = "ACME Web Application";
                  description = "Main web application for ACME services";
                  repository = "https://github.com/acme-corp/web-app.git";
                  evaluation_wildcard = "main";
                  active = true;
                  force_evaluation = false;
                  created_by = "alice";
                }
                {
                  name = "mobile-app";
                  organization = "acme-corp";
                  display_name = "ACME Mobile App";
                  description = "Mobile application for iOS and Android";
                  repository = "https://github.com/acme-corp/mobile-app.git";
                  evaluation_wildcard = "main";
                  active = true;
                  force_evaluation = false;
                  created_by = "bob";
                }
              ];

              servers = [
                {
                  name = "build-server-1";
                  display_name = "Build Server 1";
                  organization = "acme-corp";
                  active = true;
                  host = "build1.internal.acme.com";
                  port = 22;
                  username = "gradient";
                  architectures = [ "x86_64-linux" "aarch64-linux" ];
                  features = [ "big-parallel" "docker" ];
                  created_by = "alice";
                }
                {
                  name = "mac-mini-farm";
                  display_name = "Mac Mini Build Farm";
                  organization = "acme-corp";
                  active = true;
                  host = "macfarm.internal.acme.com";
                  port = 22;
                  username = "builder";
                  architectures = [ "x86_64-darwin" "aarch64-darwin" ];
                  features = [ "big-parallel" ];
                  created_by = "alice";
                }
              ];

              caches = [
                {
                  name = "main-cache";
                  display_name = "Main Binary Cache";
                  description = "Primary binary cache for all builds";
                  active = true;
                  priority = 100;
                  signing_key_file = "/etc/gradient/secrets/main_cache_key";
                  organizations = [ "acme-corp" ];
                  created_by = "alice";
                }
                {
                  name = "dev-cache";
                  display_name = "Development Cache";
                  description = "Cache for development builds";
                  active = false;
                  priority = 50;
                  signing_key_file = "/etc/gradient/secrets/dev_cache_key";
                  organizations = [ "acme-corp" ];
                  created_by = "alice";
                }
              ];

              api_keys = [{
                name = "alice_admin_key";
                key_file = "/etc/gradient/secrets/alice_api_key";
                owned_by = "alice";
              }];
            };
          };

          postgresql = {
            enable = true;
            package = pkgs.postgresql_17;
            enableJIT = true;
            enableTCPIP = true;
            ensureDatabases = [ "gradient" ];
            authentication = ''
              #...
              #type database DBuser origin-address auth-method
              # ipv4
              host  all      all     0.0.0.0/0      trust
              # ipv6
              host all       all     ::0/0        trust
            '';

            settings = {
              logging_collector = true;
              log_destination = lib.mkForce "syslog";
            };
          };
        };
      };
    };

    interactive.nodes = {
      machine = import ../../modules/debug-host.nix;
    };

    testScript = builtins.readFile ./test.py;
  });
}
