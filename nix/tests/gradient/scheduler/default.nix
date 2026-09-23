/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ self, pkgs, ... }:
let
  inherit (pkgs) lib;

  daemon = self.packages.${pkgs.stdenv.hostPlatform.system}.gradient.daemon;
  storeSpec = import ../../store-spec { inherit pkgs lib daemon; };

  # Keyed by file: chain-4 keeps the name chain-3, so it lands in chain-3's repo with c0-c2 unchanged.
  specFiles = lib.mapAttrs (_: import) {
    chain-3 = ./specs/chain.nix;
    chain-4 = ./specs/chain-4.nix;
    diamond-fail = ./specs/diamond-fail.nix;
    cross-worker = ./specs/cross-worker.nix;
    already-present = ./specs/already-present.nix;
    upstream-cached = ./specs/upstream-cached.nix;
    hang = ./specs/hang.nix;
    stress = ./specs/stress.nix;
    replay = ./specs/replay.nix;
  };
  specs = lib.attrValues specFiles;
  specNames = lib.unique (map (s: s.name) specs);

  flakes = lib.mapAttrs (_: storeSpec.toFlake) specFiles;
  resolved = lib.mapAttrs (_: storeSpec.resolve) specFiles;
  upstream = storeSpec.toUpstreamCache specs;

  workerToken = "C9ve6tvVONhtbRzFks56HQlYQotlRmXel/5NFLk/HjbSFGc+IZjCGfxegW2NKpY5";
  workerIds = {
    worker1 = "[uuid15]";
    worker2 = "[uuid16]";
  };

  workerNode = name: features: { ... }: {
    imports = [
      ../../../modules/gradient-worker.nix
      ./gradient-daemon.nix
    ];

    services.gradient-daemon-mock = {
      enable = true;
      package = daemon;
      config = storeSpec.toDaemonConfig specs name;
    };

    nix.settings.system-features = features;

    systemd.tmpfiles.rules = [
      "d /var/lib/gradient-worker 0755 gradient-worker gradient-worker"
      "f /var/lib/gradient-worker/worker-id 0644 gradient-worker gradient-worker - ${workerIds.${name}}"
    ];

    environment.etc."gradient/secrets/worker_peers" = {
      mode = "0600";
      user = "gradient-worker";
      group = "gradient-worker";
      text = "*:${workerToken}";
    };

    services.gradient.worker = {
      enable = true;
      serverUrl = "ws://server/proto";
      peersFile = "/etc/gradient/secrets/worker_peers";
      settings.systemFeatures = features;
      capabilities = {
        eval = true;
        build = true;
      };
    };
  };

  serverNode = { ... }: {
    imports = [ ../../../modules/gradient.nix ];

    environment = {
      systemPackages = with pkgs; [ curl git jq ];

      etc = {
        "gradient/secrets/admin_password" = {
          mode = "0600";
          user = "gradient";
          group = "gradient";
          text = "$argon2id$v=19$m=4096,t=3,p=1$c29tZXNhbHQxMjM0NQ$hIKBEy9SOWlnAlcwUv2PLPBdsMkKhVlCyjTxaWIK+v4";
        };

        "gradient/secrets/corp_ssh_key" = {
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

        "gradient/secrets/main_cache_key" = {
          mode = "0600";
          user = "gradient";
          group = "gradient";
          text = "22yRW7p/hxuPRWJh9pcfGH0oXPk2MFUuG0wIA1rfq1BvDbvMqzMZS+er/BE8ucbxNSG5KZ8B0ELO4TJal8mZlw==";
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
          [init]
            defaultBranch = main
        '';
      };
    };

    networking.hosts."127.0.0.1" = [ "gradient.local" ];

    services = {
      gradient = {
        enable = true;
        reverseProxy.nginx.enable = true;
        configurePostgres = true;
        postgresSharedBuffers = "128MB";
        domain = "gradient.local";
        proto.public = true;
        jwtSecretFile = toString (pkgs.writeText "jwtSecret" "b68a8eaa8ebcff23ebaba1bd74ecb8a2eb7ba959570ff8842f148207524c7b8d731d7a1998584105e951599221f9dcd20e41223be17275ca70ab6f7e6ecafa8d4f8905623866edb2b344bd15de52ccece395b3546e2f00644eb2679cf7bdaa156fd75cc5f47c34448cba19d903e68015b1ad3c8e9d04862de0a2c525b6676779012919fa9551c4746f9323ab207aedae86c28ada67c901cae821eef97b69ca4ebe1260de31add34d8265f17d9c547e3bbabe284d9cadcc22063ee625b104592403368090642a41967f8ada5791cb09703d0762a3175d0fe06ec37822e9e41d0a623a6349901749673735fdb94f2c268ac08a24216efb058feced6e785f34185a");
        cryptSecretFile = toString (pkgs.writeText "cryptSecret" "aW52YWxpZC1pbnZhbGlkLWludmFsaWQK");
        settings.logLevel.default = "info";

        state = {
          users.admin = {
            email = "admin@example.com";
            password_file = "/etc/gradient/secrets/admin_password";
            superuser = true;
          };

          projects.project = {
            private_key_file = "/etc/gradient/secrets/corp_ssh_key";
            created_by = "admin";
          };

          # No triggers: every phase starts its own evaluation through the API.
          tasks = lib.genAttrs specNames (name: {
            project = "project";
            repository = "git://server/${name}";
            created_by = "admin";
            triggers = [ ];
          });

          caches.main = {
            signing_key_file = "/etc/gradient/secrets/main_cache_key";
            projects = [ "project" ];
            public = true;
            created_by = "admin";
            upstreams = [{
              type = "external";
              display_name = "spec-upstream";
              url = "http://server/upstream";
              public_key = storeSpec.upstreamPublicKey;
            }];
          };

          workers = lib.mapAttrs (_: id: {
            worker_id = id;
            projects = [ "project" ];
            token_file = "/etc/gradient/secrets/worker_token";
            created_by = "admin";
          }) workerIds;
        };
      };

      nginx.virtualHosts."gradient.local" = {
        enableACME = lib.mkForce false;
        forceSSL = lib.mkForce false;
        locations."/upstream/" = {
          alias = "${upstream}/";
          extraConfig = "autoindex off;";
        };
      };

      postgresql = {
        package = pkgs.postgresql_18;
        enableTCPIP = true;
        authentication = ''
          host all all 0.0.0.0/0 trust
          host all all ::0/0 trust
        '';
      };

      gitDaemon = {
        enable = true;
        basePath = "/var/lib/git/";
        exportAll = true;
      };
    };

    systemd.tmpfiles.rules = [ "d /var/lib/git 0755 git git" ];
  };
in
{
  value = pkgs.testers.runNixOSTest {
    name = "gradient-scheduler";
    globalTimeout = 1800;

    defaults = {
      networking.firewall.enable = false;
      virtualisation = {
        cores = 2;
        memorySize = 2048;
        diskSize = 4096;
        writableStore = true;
      };
      documentation.enable = false;
      nix.settings.substituters = lib.mkForce [ ];
    };

    nodes = {
      server = serverNode;
      worker1 = workerNode "worker1" [ "feature-a" ];
      worker2 = workerNode "worker2" [ "feature-b" ];
    };

    testScript = ''
      FLAKES = ${builtins.toJSON flakes}
      RESOLVED = ${builtins.toJSON resolved}
      ${builtins.readFile ./test.py}
    '';
  };
}
