/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ self, pkgs, topology }: let
  e2e = ../../gradient/e2e;

  testStore = import ../../../scripts/store.nix {
    inherit pkgs;
    skipDirectories = false;
  };

  workerToken = "C9ve6tvVONhtbRzFks56HQlYQotlRmXel/5NFLk/HjbSFGc+IZjCGfxegW2NKpY5";
  workerIds.builder = "ab68d6ce-8331-44dc-9433-f22ccfd0a44b";

  captureModule = {
    environment.systemPackages = with pkgs; [ curl flamegraph jq perf strace tcpdump ];
    boot.kernel.sysctl = {
      "kernel.perf_event_paranoid" = -1;
      "kernel.kptr_restrict" = 0;
    };
  };

  builderModule = { lib, ... }: {
    imports = [ ../../../modules/gradient-worker.nix captureModule ];

    virtualisation.additionalPaths = [ testStore ];

    nix.settings = {
      trusted-users = [ "root" "@wheel" ];
      max-jobs = lib.mkForce 4;
      # No route out: every substituter lookup would cost seconds of DNS retries.
      substituters = lib.mkForce [ ];
    };

    services.gradient.worker = {
      enable = true;
      log.traceDir = "/var/lib/gradient-worker/trace";
      build.maxConcurrent = 4;
      capabilities = {
        eval = true;
        build = true;
      };
    };
  };

  topo = topology {
    inherit pkgs;
    inherit (pkgs) lib;
    token = workerToken;
    workers = pkgs.lib.mapAttrs (_: id: { inherit id; module = builderModule; }) workerIds;
  };
in
assert import ../../harness/contract.nix { inherit (pkgs) lib; topology = topo; workers = workerIds; };
pkgs.testers.runNixOSTest ({ lib, ... }: {
  name = "gradient-evalbench";
  globalTimeout = 3600;

  defaults = {
    networking.firewall.enable = false;
    virtualisation = {
      cores = 4;
      memorySize = 2048;
      diskSize = 8192;
      writableStore = true;
    };
    documentation.enable = false;
    nix.settings.max-jobs = 0;
  };

  nodes = {
    server = { pkgs, lib, ... }: {
      imports = [ ../../../modules/gradient.nix captureModule ];

      nix.settings.substituters = lib.mkForce [ ];
      networking.hosts."127.0.0.1" = [ "gradient.local" ];

      environment = {
        variables.TEST_PKGS = [ self.inputs.nixpkgs ];
        etc = {
          "gradient/secrets/admin_password" = {
            mode = "0600";
            user = "gradient";
            group = "gradient";
            text = "$argon2id$v=19$m=4096,t=3,p=1$c29tZXNhbHQxMjM0NQ$hIKBEy9SOWlnAlcwUv2PLPBdsMkKhVlCyjTxaWIK+v4";
          };

          "gradient/secrets/main_cache_key" = {
            mode = "0600";
            user = "gradient";
            group = "gradient";
            text = "22yRW7p/hxuPRWJh9pcfGH0oXPk2MFUuG0wIA1rfq1BvDbvMqzMZS+er/BE8ucbxNSG5KZ8B0ELO4TJal8mZlw==";
          };

          "gradient/secrets/project_ssh_key" = {
            mode = "0600";
            user = "gradient";
            group = "gradient";
            text = ''
            -----BEGIN OPENSSH PRIVATE KEY-----
            b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAAAMwAAAAtzc2gtZW
            QyNTUxOQAAACDle/PUDDuuI9h8+ViFyHMQjqARSRhLJcYKnay7MrflOgAAAJALQNCyC0DQ
            sgAAAAtzc2gtZWQyNTUxOQAAACDle/PUDDuuI9h8+ViFyHMQjqARSRhLJcYKnay7MrflOg
            AAAEAROowXB/e8+691yZgfHOASTPVyIM2Hx7U9RpmAtUda++V789QMO64j2Hz5WIXIcxCO
            oBFJGEslxgqdrLsyt+U6AAAABm5vbmFtZQECAwQFBgc=
            -----END OPENSSH PRIVATE KEY-----
            '';
          };

          "gradient/secrets/worker_token" = {
            mode = "0600";
            user = "gradient";
            group = "gradient";
            text = workerToken;
          };

          "gitconfig".text = ''
            [safe]
              directory = *
          '';
        };
      };

      services = {
        gradient = {
          enable = true;
          reverseProxy.nginx.enable = true;
          postgres.enable = true;
          postgres.sharedBuffers = "128MB";
          # TCP rather than the socket, so the pg capture sees every statement.
          database.url = "postgresql://gradient@127.0.0.1/gradient";
          domain = "gradient.local";
          proto.public = true;
          secrets.jwtFile = toString (pkgs.writeText "jwtSecret" "b68a8eaa8ebcff23ebaba1bd74ecb8a2eb7ba959570ff8842f148207524c7b8d731d7a1998584105e951599221f9dcd20e41223be17275ca70ab6f7e6ecafa8d4");
          secrets.cryptFile = toString (pkgs.writeText "cryptSecret" "aW52YWxpZC1pbnZhbGlkLWludmFsaWQK");
          log.traceDir = "/var/lib/gradient/trace";
          state = {
            users.admin = {
              email = "admin@example.com";
              password_file = "/etc/gradient/secrets/admin_password";
              superuser = true;
            };

            projects.project = {
              private_key_file = "/etc/gradient/secrets/project_ssh_key";
              created_by = "admin";
            };

            tasks.task = {
              project = "project";
              repository = "git://server/test";
              created_by = "admin";
              triggers = [{
                type = "time";
                config.cron = "0 0 3 1 1 *";
              }];
            };

            caches.main = {
              signing_key_file = "/etc/gradient/secrets/main_cache_key";
              projects = [ "project" ];
              public = true;
              created_by = "admin";
            };

            workers = lib.mapAttrs (_: id: {
              worker_id = id;
              projects = [ "project" ];
              token_file = "/etc/gradient/secrets/worker_token";
              created_by = "admin";
            }) topo.upstreamPeers;
          };
        };

        nginx.virtualHosts."gradient.local" = {
          enableACME = lib.mkForce false;
          forceSSL = lib.mkForce false;
        };

        postgresql = {
          package = pkgs.postgresql_18;
          enableTCPIP = true;
          authentication = ''
            host all all 127.0.0.1/32 trust
          '';
          settings = {
            shared_preload_libraries = "pg_stat_statements,auto_explain";
            "pg_stat_statements.max" = 10000;
            "auto_explain.log_min_duration" = -1;
            "auto_explain.log_nested_statements" = true;
            # acpi_pm makes every per-node clock read trap; rows and loops are enough.
            "auto_explain.log_timing" = false;
            # A plan on the serial console costs a millisecond a line, inside the
            # transaction that logged it; a file costs nothing.
            logging_collector = true;
            log_filename = "postgresql.log";
            log_rotation_age = 0;
            log_rotation_size = 0;
          };
        };

        gitDaemon = {
          enable = true;
          basePath = "/var/lib/git/";
          exportAll = true;
        };
      };

      systemd.tmpfiles.rules = [
        "d /var/lib/git 0755 git git"
        "L+ /var/lib/git/flake.nix 0755 git git - ${e2e}/flake_repository.nix"
        "L+ /var/lib/git/flake.lock 0755 git git - ${e2e}/flake_repository.lock"
      ];
    };
  } // topo.nodes;

  testScript = ''
    import time

    GIT          = "${lib.getExe pkgs.git}"
    API          = "http://gradient.local/api/v1"
    INSPECTOR    = "${lib.getExe (pkgs.callPackage ../../../tools/evalbench-inspector { })}"
    NIXPKGS      = "${self.inputs.nixpkgs}"
    NIXPKGS_HASH = "${self.inputs.nixpkgs.narHash}"
    ${import ../../harness/prelude.nix { inherit lib; topology = topo; }}
    ${builtins.readFile ./summarize.py}
    ${builtins.readFile ./capture.py}
    ${builtins.readFile ./test.py}
  '';
})
