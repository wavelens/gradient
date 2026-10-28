/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ self, pkgs, ... }: let
  inherit (pkgs) lib;

  nixpkgsTarball = import ../../harness/nixpkgs-tarball.nix { inherit pkgs; inherit (self.inputs) nixpkgs; };
  programs = import ./apps.nix { inherit pkgs; };
  workerToken = "C9ve6tvVONhtbRzFks56HQlYQotlRmXel/5NFLk/HjbSFGc+IZjCGfxegW2NKpY5";
  workerIds = {
    stopper = "a0000000-0000-0000-0000-000000000011";
    keeper = "a0000000-0000-0000-0000-000000000012";
  };
  guestSubnets = {
    stopper = "10.233.0.0";
    keeper = "10.234.0.0";
  };

  guestImage = self.lib.mkWorkerImage {
    modules = [
      ({ pkgs, ... }: {
        gradient.guest = {
          cpus = 1;
          memoryMb = 512;
          preScript = ''
            ${pkgs.systemd}/bin/systemd-run --unit=gradient-test-agent-restart ${pkgs.runtimeShell} -c '
              while [ ! -e /work/restart-agent ]; do ${pkgs.coreutils}/bin/sleep 1; done
              ${pkgs.systemd}/bin/systemctl restart gradient-guest-agent
              ${pkgs.coreutils}/bin/rm /work/restart-agent
            '
          '';
        };
      })
    ];
  };

  sopsFixtures = ../../../../backend/gradient-sops/tests/fixtures;
  ageRecipient = lib.fileContents "${sopsFixtures}/ed25519.age-recipient";
  clashSops = pkgs.runCommand "clash.yaml" { nativeBuildInputs = [ pkgs.sops ]; } ''
    export HOME=$TMPDIR
    printf 'clash-key: clash-value-1\n' > plain.yaml
    sops encrypt --age ${ageRecipient} --input-type yaml --output-type yaml plain.yaml > $out
  '';

  secretFile = text: {
    mode = "0600";
    user = "gradient";
    group = "gradient";
    inherit text;
  };

  execWorker = { name, peer, label, maxPreWarm, stopGuestsWithWorker, memorySize ? 4096, sops ? { } }: { nodes, pkgs, lib, ... }: {
    imports = [ ../../../modules/gradient-worker.nix ];

    virtualisation = {
      cores = 2;
      inherit memorySize;
      diskSize = 8192;
      writableStore = true;
    };

    networking = {
      nameservers = [ nodes.server.networking.primaryIPAddress ];
      interfaces.eth1.ipv4.routes = [{
        address = guestSubnets.${peer};
        prefixLength = 16;
        via = nodes.${peer}.networking.primaryIPAddress;
      }];
    };
    nix.settings.substituters = lib.mkForce [ ];
    environment.systemPackages = [ pkgs.curl pkgs.jq ];

    environment.etc."gradient/secrets/worker_peers" = {
      mode = "0600";
      user = "gradient-worker";
      group = "gradient-worker";
      text = "*:${workerToken}";
    };

    services.gradient.worker = {
      enable = true;
      serverUrl = "ws://server/proto";
      id = workerIds.${name};
      zone = "lab";
      peersFile = "/etc/gradient/secrets/worker_peers";
      capabilities = {
        eval = true;
        build = true;
        exec = true;
      };
      exec = {
        image = guestImage;
        preWarm = 1;
        labels = [ label "pair" ];
        subnet = "${guestSubnets.${name}}/16";
        inherit maxPreWarm stopGuestsWithWorker sops;
        allow = {
          network = true;
          ssh = true;
          retries = 1;
          nodes = 2;
        };
      };
    };
  };
in {
  value = pkgs.testers.runNixOSTest ({ pkgs, lib, ... }: {
    name = "gradient-exec";
    globalTimeout = 2700;

    defaults.documentation.enable = false;

    nodes = {
      server = { config, pkgs, lib, ... }: {
        imports = [ ../../../modules/gradient.nix ];

        virtualisation = {
          cores = 2;
          memorySize = 2048;
          diskSize = 8192;
          writableStore = true;
          additionalPaths = lib.attrValues programs;
        };

        networking = {
          firewall.enable = false;
          hosts."127.0.0.1" = [ "gradient.local" ];
        };

        nix.settings.substituters = lib.mkForce [ ];
        environment.systemPackages = [ pkgs.git pkgs.curl pkgs.jq pkgs.openssh ];

        environment.etc = {
          "gradient/secrets/admin_password" = secretFile "$argon2id$v=19$m=4096,t=3,p=1$c29tZXNhbHQxMjM0NQ$hIKBEy9SOWlnAlcwUv2PLPBdsMkKhVlCyjTxaWIK+v4";
          "gradient/secrets/worker_token" = secretFile workerToken;
          "gradient/secrets/main_cache_key" = secretFile "22yRW7p/hxuPRWJh9pcfGH0oXPk2MFUuG0wIA1rfq1BvDbvMqzMZS+er/BE8ucbxNSG5KZ8B0ELO4TJal8mZlw==";
          "gradient/secrets/project_ssh_key" = secretFile (builtins.readFile "${sopsFixtures}/ed25519");
          "gradient/secrets/api_token" = secretFile "api-token-value-1";
          "gradient/secrets/upstream_key" = {
            mode = "0600";
            text = "file-upstream-1:eZPukjHYgRpJ+hLlnUgG+qpi/k4QMTr3bd4ftngZJwkIXutyHrlDclZGwczy+IKCAt82HkKrDtM16u9HR9uzkQ==";
          };
          "gitconfig".text = ''
            [safe]
              directory = *
          '';
        };

        services = {
          gradient = {
            enable = true;
            reverseProxy.nginx.enable = true;
            postgres.enable = true;
            domain = "gradient.local";
            useTls = false;
            proto.public = true;
            ssh.enable = true;
            secrets.jwtFile = toString (pkgs.writeText "jwtSecret" "b68a8eaa8ebcff23ebaba1bd74ecb8a2eb7ba959570ff8842f148207524c7b8d731d7a1998584105e951599221f9dcd20e41223be17275ca70ab6f7e6ecafa8d4");
            secrets.cryptFile = toString (pkgs.writeText "cryptSecret" "aW52YWxpZC1pbnZhbGlkLWludmFsaWQK");

            state = {
              users.admin = {
                email = "admin@example.com";
                password_file = "/etc/gradient/secrets/admin_password";
                superuser = true;
              };

              projects.project = {
                private_key_file = "/etc/gradient/secrets/project_ssh_key";
                created_by = "admin";
                exec_enabled = true;
                secret_files.api-token = "/etc/gradient/secrets/api_token";
              };

              tasks.task = {
                project = "project";
                repository = "git://server/test";
                wildcard = "apps.x86_64-linux.*";
                created_by = "admin";
                triggers = [{
                  type = "polling";
                  config.interval_secs = 10;
                }];
              };

              caches.main = {
                signing_key_file = "/etc/gradient/secrets/main_cache_key";
                projects = [ "project" ];
                public = true;
                created_by = "admin";
                upstream_caches = [{
                  type = "external";
                  display_name = "file-upstream";
                  url = "http://server/upstream";
                  public_key = "file-upstream-1:CF7rch65Q3JWRsHM8viCggLfNh5Cqw7TNervR0fbs5E=";
                }];
              };

              workers = lib.mapAttrs (_: id: {
                worker_id = id;
                projects = [ "project" ];
                token_file = "/etc/gradient/secrets/worker_token";
                created_by = "admin";
                enable_exec = true;
              }) workerIds;
            };
          };

          nginx.virtualHosts = {
            "gradient.local" = {
              enableACME = lib.mkForce false;
              forceSSL = lib.mkForce false;
              locations."/upstream/" = {
                alias = "/srv/upstream/";
                extraConfig = "autoindex off;";
              };
              locations."= /nixpkgs.tar.gz".alias = nixpkgsTarball;
            };

            "web.exec.test" = {
              listen = [{ addr = "0.0.0.0"; port = 8080; }];
              locations."/".extraConfig = ''
                default_type text/plain;
                return 200 "served to the guest\n";
              '';
            };
          };

          dnsmasq = {
            enable = true;
            resolveLocalQueries = false;
            settings = {
              no-resolv = true;
              no-hosts = true;
              address = map (name: "/${name}/${config.networking.primaryIPAddress}") [ "web.exec.test" "gradient.local" ];
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
          "d /srv/upstream 0755 nginx nginx"
          "L+ /var/lib/git/flake.nix - - - - ${./flake_repository.nix}"
          "L+ /var/lib/git/flake.lock - - - - ${../e2e/flake_repository.lock}"
          "L+ /var/lib/git/apps.nix - - - - ${./apps.nix}"
          "L+ /var/lib/git/train.yaml - - - - ${sopsFixtures}/secrets.yaml"
        ];
      };

      stopper = execWorker {
        name = "stopper";
        peer = "keeper";
        label = "stop";
        maxPreWarm = 2;
        stopGuestsWithWorker = true;
        memorySize = 6144;
        sops = {
          files = [ "${sopsFixtures}/worker.yaml" "${clashSops}" ];
          keyFile = "${sopsFixtures}/ed25519";
        };
      };
      keeper = execWorker {
        name = "keeper";
        peer = "stopper";
        label = "keep";
        maxPreWarm = 0;
        stopGuestsWithWorker = false;
      };
    };

    interactive.nodes = lib.genAttrs [ "server" "stopper" "keeper" ] (_: import ../../modules/debug-host.nix);

    testScript = ''
      GIT = "${lib.getExe pkgs.git}"
      NIXPKGS_HASH = "${self.inputs.nixpkgs.narHash}"
      PROGRAMS = "${lib.concatStringsSep " " (lib.attrValues programs)}"
      AGE_RECIPIENT = "${ageRecipient}"
      STOPPER_ID = "${workerIds.stopper}"
    '' + builtins.readFile ./test.py;
  });
}
