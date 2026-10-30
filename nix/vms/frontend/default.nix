/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ inputs, pkgs, lib, ... }: let
  nixpkgsTarball = import ../../tests/harness/nixpkgs-tarball.nix { inherit pkgs; inherit (inputs) nixpkgs; };
  execPrograms = import ./exec_apps.nix { inherit pkgs; };
  guestImage = inputs.self.lib.mkWorkerImage {
    modules = [ { gradient.guest = { cpus = 1; memoryMb = 512; }; } ];
  };
  upstreamKey = pkgs.writeText "upstream-key" "file-upstream-1:eZPukjHYgRpJ+hLlnUgG+qpi/k4QMTr3bd4ftngZJwkIXutyHrlDclZGwczy+IKCAt82HkKrDtM16u9HR9uzkQ==";

  # These are the fixture secrets of the integration tests. The admin password is "admin_password".
  adminPwHash = pkgs.writeText "admin-pw-hash" "$argon2id$v=19$m=4096,t=3,p=1$c29tZXNhbHQxMjM0NQ$hIKBEy9SOWlnAlcwUv2PLPBdsMkKhVlCyjTxaWIK+v4";
  projectSshKey = pkgs.writeText "project-ssh-key" ''
    -----BEGIN OPENSSH PRIVATE KEY-----
    b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAAAMwAAAAtzc2gtZW
    QyNTUxOQAAACDle/PUDDuuI9h8+ViFyHMQjqARSRhLJcYKnay7MrflOgAAAJALQNCyC0DQ
    sgAAAAtzc2gtZWQyNTUxOQAAACDle/PUDDuuI9h8+ViFyHMQjqARSRhLJcYKnay7MrflOg
    AAAEAROowXB/e8+691yZgfHOASTPVyIM2Hx7U9RpmAtUda++V789QMO64j2Hz5WIXIcxCO
    oBFJGEslxgqdrLsyt+U6AAAABm5vbmFtZQECAwQFBgc=
    -----END OPENSSH PRIVATE KEY-----
  '';
  workerToken = pkgs.writeText "worker-token" "C9ve6tvVONhtbRzFks56HQlYQotlRmXel/5NFLk/HjbSFGc+IZjCGfxegW2NKpY5";
  cacheSigningKey = pkgs.writeText "cache-signing-key" "22yRW7p/hxuPRWJh9pcfGH0oXPk2MFUuG0wIA1rfq1BvDbvMqzMZS+er/BE8ucbxNSG5KZ8B0ELO4TJal8mZlw==";
in {
  name = "development-frontend";
  globalTimeout = 604800;
  testScript = { nodes, ... }: ''
    start_all()
    machine.wait_for_unit("gradient-server.service")
    machine.wait_for_unit("git-daemon.service")
    machine.wait_for_open_port(3000)
    machine.wait_for_unit("nginx.service")

    machine.succeed("${lib.getExe pkgs.curl} http://gradient.local/api/v1/health -i --fail")

    machine.succeed("${lib.getExe pkgs.git} config --global --add safe.directory '*'")
    machine.succeed("${lib.getExe pkgs.git} config --global init.defaultBranch main")
    machine.succeed("${lib.getExe pkgs.git} config --global user.email 'nixos@localhost'")
    machine.succeed("${lib.getExe pkgs.git} config --global user.name 'NixOS test'")

    machine.succeed("${lib.getExe pkgs.git} init /var/lib/git/test")
    machine.succeed("cp /var/lib/git/{,test/}flake.nix")
    machine.succeed("chown git:git -R /var/lib/git/test")
    machine.succeed("${lib.getExe pkgs.git} -C /var/lib/git/test add flake.nix")
    machine.succeed("${lib.getExe pkgs.git} -C /var/lib/git/test commit -m 'Initial commit'")

    machine.succeed(
        "nix --extra-experimental-features nix-command copy "
        "--to 'file:///srv/upstream?secret-key=${upstreamKey}' "
        "--no-check-sigs ${lib.concatStringsSep " " (lib.attrValues execPrograms)}"
    )
    machine.succeed("chown -R nginx:nginx /srv/upstream")

    machine.succeed("${lib.getExe pkgs.git} init /var/lib/git/exec")
    machine.succeed("cp /var/lib/git/exec-src/{flake.nix,flake.lock,exec_apps.nix} /var/lib/git/exec/")
    machine.succeed("chmod u+w /var/lib/git/exec/*")
    machine.succeed("sed -i -e 's#\\[hash\\]#${inputs.nixpkgs.narHash}#g' -e 's#//server/#//gradient.local/#g' /var/lib/git/exec/flake.lock")
    machine.succeed("${lib.getExe pkgs.git} -C /var/lib/git/exec add flake.nix flake.lock exec_apps.nix")
    machine.succeed("${lib.getExe pkgs.git} -C /var/lib/git/exec commit -m 'Exec apps'")
    machine.succeed("chown git:git -R /var/lib/git/exec")

    machine.wait_for_unit("gradient-worker.service")

    print("Dev environment ready: frontend proxy target http://localhost:3000")
    print("Login: admin / admin_password, alice / admin_password")
  '';

  interactive = {
    sshBackdoor.enable = true;
    nodes.machine.virtualisation.graphics = false;
  };

  nodes.machine = { config, pkgs, lib, ... }: {
    imports = [
      ../../modules/gradient.nix
      ../../modules/gradient-worker.nix
    ];

    networking = {
      hosts = {
        "127.0.0.1" = [ "gradient.local" ];
      };

      firewall.enable = false;
    };
    documentation.enable = false;
    virtualisation = {
      cores = 4;
      memorySize = 8192;
      diskSize = 16384;
      writableStore = true;
      additionalPaths = lib.attrValues execPrograms;
      forwardPorts = [
        {
          from = "host";
          host.port = 2222;
          guest.port = 22;
        }
        {
          from = "host";
          host.port = 3000;
          guest.port = 80;
        }
      ];
    };

    nix.settings = {
      substituters = lib.mkForce [ ];
      max-jobs = lib.mkForce 4;
    };

    systemd.tmpfiles.rules = [
      "d /var/lib/git 0755 git git"
      "L+ /var/lib/git/flake.nix 0755 git git - ${./flake_repository.nix}"
      "d /var/lib/git/exec-src 0755 git git"
      "L+ /var/lib/git/exec-src/flake.nix - - - - ${./exec_repository.nix}"
      "L+ /var/lib/git/exec-src/flake.lock - - - - ${../../tests/gradient/e2e/flake_repository.lock}"
      "L+ /var/lib/git/exec-src/exec_apps.nix - - - - ${./exec_apps.nix}"
      "d /srv/upstream 0755 nginx nginx"
      "d /var/lib/gradient-worker 0755 gradient-worker gradient-worker"
      "f /var/lib/gradient-worker/worker-id 0644 gradient-worker gradient-worker - a0000000-0000-0000-0000-000000000001"
    ];

    environment.etc = {
      "gradient/secrets/worker_peers" = {
        mode = "0600";
        user = "gradient-worker";
        group = "gradient-worker";
        text = "*:C9ve6tvVONhtbRzFks56HQlYQotlRmXel/5NFLk/HjbSFGc+IZjCGfxegW2NKpY5";
      };

      # git-daemon is running as nobody and must read repos owned by other users.
      "gitconfig".text = ''
        [safe]
          directory = *
      '';
    };

    security.pam.services.sshd.allowNullPassword = true;
    services = {
      gradient = {
        enable = true;
        reverseProxy.nginx.enable = true;
        useTls = false;
        localWorker = false;
        postgres.enable = true;
        domain = "gradient.local";
        frontend.enable = false;
        proto.public = true;
        secrets = {
          jwtFile =toString (pkgs.writeText "jwtSecret" "b68a8eaa8ebcff23ebaba1bd74ecb8a2eb7ba959570ff8842f148207524c7b8d731d7a1998584105e951599221f9dcd20e41223be17275ca70ab6f7e6ecafa8d4f8905623866edb2b344bd15de52ccece395b3546e2f00644eb2679cf7bdaa156fd75cc5f47c34448cba19d903e68015b1ad3c8e9d04862de0a2c525b6676779012919fa9551c4746f9323ab207aedae86c28ada67c901cae821eef97b69ca4ebe1260de31add34d8265f17d9c547e3bbabe284d9cadcc22063ee625b104592403368090642a41967f8ada5791cb09703d0762a3175d0fe06ec37822e9e41d0a623a6349901749673735fdb94f2c268ac08a24216efb058feced6e785f34185a");
          cryptFile = toString (pkgs.writeText "cryptSecret" "aW52YWxpZC1pbnZhbGlkLWludmFsaWQK");
        };

        state = {
          users = {
            admin = {
              email = "admin@gradient.local";
              password_file = toString adminPwHash;
              email_verified = true;
              superuser = true;
            };

            alice = {
              name = "Alice Example";
              email = "alice@gradient.local";
              password_file = toString adminPwHash;
              email_verified = true;
            };
          };

          teams = {
            testteam = {
              display_name = "MyTeam";
              members = [
                { user = "admin"; role = "Admin"; }
                { user = "alice"; role = "Member"; }
              ];
              new_projects = {
                users = true;
                workers = true;
                role = "Admin";
              };
            };

            research = {
              display_name = "Research";
              members = [
                { user = "alice"; role = "Admin"; }
                { user = "admin"; role = "Member"; }
              ];
            };
          };

          projects = {
            testproject = {
              display_name = "MyProject";
              description = "My Test Project";
              private_key_file = toString projectSshKey;
              public = true;
              exec_enabled = true;
              created_by = "admin";
              teams = [ { team = "testteam"; role = "Admin"; } ];
            };

            researchproject = {
              display_name = "Research Project";
              description = "Members of the Research team own this private project.";
              private_key_file = toString projectSshKey;
              created_by = "alice";
              teams = [
                { team = "research"; role = "Admin"; }
                { team = "testteam"; role = "View"; }
              ];
            };
          };

          tasks = {
            failingtask = {
              project = "researchproject";
              display_name = "Failing Task";
              description = "One build failed in this task.";
              repository = "git://gradient.local/test";
              wildcard = "packages.**.buildFail,packages.**.lib1";
              created_by = "alice";
              triggers = [
                {
                  type = "polling";
                  config = { interval_secs = 300; };
                }
              ];
            };

            exectask = {
              project = "testproject";
              display_name = "Exec Task";
              description = "One exec job per task.";
              repository = "git://gradient.local/exec";
              wildcard = "apps.x86_64-linux.*";
              exec_enabled = true;
              created_by = "admin";
              triggers = [
                {
                  type = "polling";
                  config = { interval_secs = 600; };
                }
              ];
            };

            testtask = {
              project = "testproject";
              display_name = "MyTask";
              description = "My Test Task";
              repository = "git://gradient.local/test";
              wildcard = "packages.**.buildWait5Sec,packages.**.deployment";
              keep_evaluations = 10;
              created_by = "admin";
              triggers = [
                {
                  type = "polling";
                  config = { interval_secs = 30; };
                }
              ];
            };
          };

          caches = {
            testcache = {
              display_name = "MyCache";
              signing_key_file = toString cacheSigningKey;
              projects = [ "testproject" ];
              public = true;
              created_by = "admin";
              upstream_caches = [{
                type = "external";
                display_name = "file-upstream";
                url = "http://gradient.local/upstream";
                public_key = "file-upstream-1:CF7rch65Q3JWRsHM8viCggLfNh5Cqw7TNervR0fbs5E=";
              }];
            };

            researchcache = {
              display_name = "Research Cache";
              description = "A private cache can use MyCache as upstream cache.";
              signing_key_file = toString cacheSigningKey;
              projects = [ "researchproject" ];
              created_by = "alice";
              teams = [ { team = "research"; role = "Admin"; } ];
              upstream_caches = [{
                type = "internal";
                cache_name = "testcache";
              }];
            };
          };

          workers.devworker = {
            worker_id = "a0000000-0000-0000-0000-000000000001";
            team = "testteam";
            token_file = toString workerToken;
            display_name = "Dev Worker";
            created_by = "admin";
            enable_exec = true;
          };
        };

        worker = {
          enable = true;
          serverUrl = "ws://gradient.local/proto";
          peersFile = "/etc/gradient/secrets/worker_peers";
          capabilities = {
            eval = true;
            build = true;
            exec = true;
          };
          exec.image = guestImage;
        };
      };

      nginx.virtualHosts."gradient.local".locations = {
        "/upstream/" = {
          alias = "/srv/upstream/";
          extraConfig = "autoindex off;";
        };
        "= /nixpkgs.tar.gz".alias = nixpkgsTarball;
      };

      postgresql = {
        package = pkgs.postgresql_18;
        enableTCPIP = true;
        authentication = ''
          host  all      all     0.0.0.0/0      trust
          host all       all     ::0/0        trust
        '';

        settings = {
          log_connections = true;
          logging_collector = true;
          log_disconnections = true;
          log_destination = lib.mkForce "syslog";
        };
      };

      gitDaemon = {
        enable = true;
        basePath = "/var/lib/git/";
        exportAll = true;
        options = "--enable=receive-pack";
      };

      openssh = {
        enable = true;
        settings = {
          PermitRootLogin = "yes";
          PermitEmptyPasswords = "yes";
        };
      };
    };
  };
}
