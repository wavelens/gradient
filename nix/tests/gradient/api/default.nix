/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ pkgs, ... }: let
  # Declarative-state fixtures (#347). Secret formats are the ones the cache
  # test already proves the provisioner accepts; the API key file holds the
  # SHA-256 of the raw token so the bearer `GRAD${stateApiKeyRaw}` authorizes.
  statePwHash = pkgs.writeText "state-pw-hash" "$argon2id$v=19$m=4096,t=3,p=1$c29tZXNhbHQxMjM0NQ$hIKBEy9SOWlnAlcwUv2PLPBdsMkKhVlCyjTxaWIK+v4";
  stateSshKey = pkgs.writeText "state-ssh-key" ''
    -----BEGIN OPENSSH PRIVATE KEY-----
    b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAAAMwAAAAtzc2gtZW
    QyNTUxOQAAACDle/PUDDuuI9h8+ViFyHMQjqARSRhLJcYKnay7MrflOgAAAJALQNCyC0DQ
    sgAAAAtzc2gtZWQyNTUxOQAAACDle/PUDDuuI9h8+ViFyHMQjqARSRhLJcYKnay7MrflOg
    AAAEAROowXB/e8+691yZgfHOASTPVyIM2Hx7U9RpmAtUda++V789QMO64j2Hz5WIXIcxCO
    oBFJGEslxgqdrLsyt+U6AAAABm5vbmFtZQECAwQFBgc=
    -----END OPENSSH PRIVATE KEY-----
  '';
  stateSigningKey = pkgs.writeText "state-cache-key" "22yRW7p/hxuPRWJh9pcfGH0oXPk2MFUuG0wIA1rfq1BvDbvMqzMZS+er/BE8ucbxNSG5KZ8B0ELO4TJal8mZlw==";
  stateWorkerToken = pkgs.writeText "state-worker-token" "C9ve6tvVONhtbRzFks56HQlYQotlRmXel/5NFLk/HjbSFGc+IZjCGfxegW2NKpY5";
  stateIntSecret = pkgs.writeText "state-int-secret" "C9ve6tvVONhtbRzFks56HQlYQotlRmXel/5NFLk/HjbSFGc+IZjCGfxegW2NKpY5";
  stateApiKeyRaw = "statecitokenrawvalue";
  stateApiKeyHash = pkgs.writeText "state-api-key" (builtins.hashString "sha256" stateApiKeyRaw);
in {
  value = pkgs.testers.runNixOSTest ({ pkgs, lib, ... }: {
    name = "gradient-api";
    globalTimeout = 600;

    nodes = {
      machine = { config, pkgs, lib, ... }: {
        imports = [ ../../../modules/gradient.nix ];

        networking.hosts."127.0.0.1" = [ "gradient.local" ];

        environment.systemPackages = with pkgs; [
          curl
          jq
          gradient-cli
        ];

        services = {
          gradient = {
            enable = true;
            reverseProxy.nginx.enable = true;
            configurePostgres = true;
            domain = "gradient.local";
            jwtSecretFile = toString (pkgs.writeText "jwtSecret" "b68a8eaa8ebcff23ebaba1bd74ecb8a2eb7ba959570ff8842f148207524c7b8d731d7a1998584105e951599221f9dcd20e41223be17275ca70ab6f7e6ecafa8d4f8905623866edb2b344bd15de52ccece395b3546e2f00644eb2679cf7bdaa156fd75cc5f47c34448cba19d903e68015b1ad3c8e9d04862de0a2c525b6676779012919fa9551c4746f9323ab207aedae86c28ada67c901cae821eef97b69ca4ebe1260de31add34d8265f17d9c547e3bbabe284d9cadcc22063ee625b104592403368090642a41967f8ada5791cb09703d0762a3175d0fe06ec37822e9e41d0a623a6349901749673735fdb94f2c268ac08a24216efb058feced6e785f34185a");
            cryptSecretFile = toString (pkgs.writeText "cryptSecret" "aW52YWxpZC1pbnZhbGlkLWludmFsaWQK");
            settings.logLevel.default = "debug";

            state = {
              users = {
                stateadmin = {
                  email = "stateadmin@gradient.local";
                  password_file = toString statePwHash;
                  email_verified = true;
                  superuser = true;
                };
                statemember = {
                  email = "statemember@gradient.local";
                  password_file = toString statePwHash;
                };
              };

              organizations.stateorg = {
                display_name = "State Org";
                description = "Provisioned by state";
                private_key_file = toString stateSshKey;
                public = true;
                created_by = "stateadmin";
                members = [
                  { user = "stateadmin"; role = "Admin"; }
                  { user = "statemember"; role = "releaser"; }
                ];
              };

              roles.releaser = {
                organization = "stateorg";
                permissions = [ "viewOrg" "triggerEvaluation" ];
              };

              projects.stateproject = {
                organization = "stateorg";
                display_name = "State Project";
                repository = "git@github.com:Wavelens/Gradient.git";
                wildcard = "packages.x86_64-linux.*";
                concurrency = "hard_abort";
                sign_cache = false;
                keep_evaluations = 5;
                created_by = "stateadmin";
                triggers = [
                  { type = "polling"; config = { interval_secs = 300; }; }
                  { type = "time"; config = { cron = "0 0 2 * * *"; }; }
                ];
                flake_input_overrides.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
              };

              caches.statecache = {
                display_name = "State Cache";
                signing_key_file = toString stateSigningKey;
                organizations = [ "stateorg" ];
                public = true;
                priority = 20;
                local_priority = 5;
                max_storage_gb = 5;
                created_by = "stateadmin";
                members = [ { user = "statemember"; role = "View"; } ];
                roles = [ { name = "cachereaders"; permissions = [ "viewCache" ]; } ];
                upstreams = [{
                  type = "external";
                  display_name = "cache.nixos.org";
                  url = "https://cache.nixos.org";
                  public_key = "cache.nixos.org-1:6NCHdD59X431o0gWypbMrAURkbJ16ZPMQFGspcDShjY=";
                }];
              };

              api_keys.state-ci-key = {
                key_file = toString stateApiKeyHash;
                owned_by = "statemember";
                permissions = [ "viewOrg" ];
                organization = "stateorg";
              };

              workers.stateworker = {
                worker_id = "a0000000-0000-0000-0000-0000000000aa";
                organizations = [ "stateorg" ];
                token_file = toString stateWorkerToken;
                display_name = "State Worker";
                created_by = "stateadmin";
                enable_fetch = false;
              };

              integrations = {
                state-inbound = {
                  organization = "stateorg";
                  kind = "inbound";
                  forge_type = "gitea";
                  secret_file = toString stateIntSecret;
                  created_by = "stateadmin";
                };
                state-outbound = {
                  organization = "stateorg";
                  kind = "outbound";
                  forge_type = "gitea";
                  endpoint_url = "https://gitea.example.com";
                  access_token_file = toString stateIntSecret;
                  created_by = "stateadmin";
                };
              };
            };
          };

          nginx.virtualHosts."gradient.local" = {
            enableACME = lib.mkForce false;
            forceSSL = lib.mkForce false;
          };

          postgresql = {
            enable = true;
            package = pkgs.postgresql_18;
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
              log_connections = true;
              logging_collector = true;
              log_disconnections = true;
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
