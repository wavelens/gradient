/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ self, pkgs, ... }: {
  value = pkgs.testers.runNixOSTest ({ pkgs, lib, ... }: {
    name = "gradient-cache";
    globalTimeout = 960;

    defaults = {
      networking.firewall.enable = false;
      virtualisation.writableStore = true;
      documentation.enable = false;
      nix.settings.max-jobs = 0;
    };

    nodes = {
      server = { config, pkgs, lib, ... }: {
        imports = [
          ../../../modules/gradient.nix
        ];

        nix.settings.substituters = lib.mkForce [ ];
        environment = {
          variables.TEST_PKGS = [ self.inputs.nixpkgs ];
          systemPackages = with pkgs; [
            binutils
            busybox
            coreutils
            hello
            stdenv
          ];

          etc = {
            "gradient/secrets/admin_password" = {
              mode = "0600";
              user = "gradient";
              group = "gradient";
              text = "admin_password";
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
              text = "C9ve6tvVONhtbRzFks56HQlYQotlRmXel/5NFLk/HjbSFGc+IZjCGfxegW2NKpY5";
            };
          };
        };

        networking.hosts = {
          "127.0.0.1" = [ "gradient.local" ];
        };

        services = {
          gradient = {
            enable = true;
            configureNginx = true;
            configurePostgres = true;
            domain = "gradient.local";
            jwtSecretFile = toString (pkgs.writeText "jwtSecret" "b68a8eaa8ebcff23ebaba1bd74ecb8a2eb7ba959570ff8842f148207524c7b8d731d7a1998584105e951599221f9dcd20e41223be17275ca70ab6f7e6ecafa8d4f8905623866edb2b344bd15de52ccece395b3546e2f00644eb2679cf7bdaa156fd75cc5f47c34448cba19d903e68015b1ad3c8e9d04862de0a2c525b6676779012919fa9551c4746f9323ab207aedae86c28ada67c901cae821eef97b69ca4ebe1260de31add34d8265f17d9c547e3bbabe284d9cadcc22063ee625b104592403368090642a41967f8ada5791cb09703d0762a3175d0fe06ec37822e9e41d0a623a6349901749673735fdb94f2c268ac08a24216efb058feced6e785f34185a");
            cryptSecretFile = toString (pkgs.writeText "cryptSecret" "aW52YWxpZC1pbnZhbGlkLWludmFsaWQK");
            settings.logLevel.default = "debug";
            state = {
              users = {
                admin = {
                  email = "admin@example.com";
                  password_file = "/etc/gradient/secrets/admin_password";
                };
              };

              organizations = {
                org = {
                  private_key_file = "/etc/gradient/secrets/corp_ssh_key";
                  created_by = "admin";
                };
              };

              projects = {
                project = {
                  organization = "org";
                  repository = "git://server/test";
                  created_by = "admin";
                };
              };

              caches = {
                main = {
                  signing_key_file = "/etc/gradient/secrets/main_cache_key";
                  organizations = [ "org" ];
                  public = true;
                  created_by = "admin";
                };
              };

              workers = {
                builder = {
                  worker_id = "a0000000-0000-0000-0000-000000000001";
                  organization = "org";
                  token_file = "/etc/gradient/secrets/worker_token";
                };
              };
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

          gitDaemon = {
            enable = true;
            basePath = "/var/lib/git/";
            exportAll = true;
            options = "--enable=receive-pack";
          };
        };

        # Allow git-daemon (runs as nobody) to access repos owned by other users.
        environment.etc."gitconfig".text = ''
          [safe]
            directory = *
        '';

        systemd.tmpfiles.rules = [
          "d /var/lib/git 0755 git git"
          "L+ /var/lib/git/flake.nix 0755 git git - ${./flake_repository.nix}"
          "L+ /var/lib/git/flake.lock 0755 git git - ${./flake_repository.lock}"
        ];
      };

      builder = { config, pkgs, lib, ... }: {
        imports = [ ../../../modules/gradient-worker.nix ];

        nix.settings = {
          trusted-users = [
            "root"
            "@wheel"
          ];

          max-jobs = lib.mkForce 8;
        };

        # Pre-seed a deterministic worker UUID so the server state config
        # can register it before the worker boots.
        systemd.tmpfiles.rules = [
          "d /var/lib/gradient-worker 0755 gradient-worker gradient-worker"
          "f /var/lib/gradient-worker/worker-id 0644 gradient-worker gradient-worker - a0000000-0000-0000-0000-000000000001"
        ];

        environment.etc."gradient/secrets/worker_peers" = {
          mode = "0600";
          user = "gradient-worker";
          group = "gradient-worker";
          text = "*:C9ve6tvVONhtbRzFks56HQlYQotlRmXel/5NFLk/HjbSFGc+IZjCGfxegW2NKpY5";
        };

        services.gradient.worker = {
          enable = true;
          serverUrl = "ws://server/proto";
          peersFile = "/etc/gradient/secrets/worker_peers";
          capabilities = {
            eval  = true;
            build = true;
          };
        };
      };

      client = { config, pkgs, lib, ... }: {
        environment.variables.TEST_PKGS = [ self.inputs.nixpkgs ];
        nix.settings = {
          substituters = lib.mkForce [ "http://server/cache/main" ];
          trusted-public-keys = lib.mkForce [ "gradient.local-main:bw27zKszGUvnq/wRPLnG8TUhuSmfAdBCzuEyWpfJmZc=" ];
        };
      };
    };

    interactive.nodes = {
      server  = import ../../modules/debug-host.nix;
      builder = import ../../modules/debug-host.nix;
      client  = import ../../modules/debug-host.nix;
    };

    testScript = { nodes, ... }:
      ''
      start_all()

      server.wait_for_unit("gradient-server.service")
      server.sleep(5)
      builder.wait_for_unit("gradient-worker.service")

      # Verify worker authenticated via state-managed registration
      builder.sleep(10)
      auth_logs = builder.succeed("journalctl -u gradient-worker --no-pager -n 100")
      assert "handshake successful" in auth_logs, \
          f"Worker did not authenticate successfully: {auth_logs[-500:]}"
      print("=== Worker authenticated via state-managed registration ===")

      # Configure git
      server.succeed("${lib.getExe pkgs.git} config --global --add safe.directory '*'")
      server.succeed("${lib.getExe pkgs.git} config --global init.defaultBranch main")
      server.succeed("${lib.getExe pkgs.git} config --global user.email 'nixos@localhost'")
      server.succeed("${lib.getExe pkgs.git} config --global user.name 'NixOS test'")

      # Initialize git repository
      server.succeed("${lib.getExe pkgs.git} init /var/lib/git/test")
      server.succeed("cp /var/lib/git/{,test/}flake.nix")
      server.succeed("cp /var/lib/git/{,test/}flake.lock")

      server.succeed("sed -i 's#\\[nixpkgs\\]#${self.inputs.nixpkgs}#g' /var/lib/git/test/flake.nix")
      server.succeed("sed -i 's#\\[nixpkgs\\]#${self.inputs.nixpkgs}#g' /var/lib/git/test/flake.lock")

      nixpkgs_hash = server.succeed("${lib.getExe pkgs.nix} hash path ${self.inputs.nixpkgs}").strip()
      server.succeed(f"sed -i 's#\\[hash\\]#{nixpkgs_hash}#g' /var/lib/git/test/flake.lock")

      server.succeed("${lib.getExe pkgs.git} -C /var/lib/git/test add flake.nix")
      server.succeed("${lib.getExe pkgs.git} -C /var/lib/git/test add flake.lock")
      server.succeed("${lib.getExe pkgs.git} -C /var/lib/git/test commit -m 'Initial commit'")
      server.succeed("chown git:git -R /var/lib/git/test")

      # Ensure git repository is available without authentication
      server.succeed("${lib.getExe pkgs.git} clone git://localhost/test test")
      print(server.succeed("${lib.getExe pkgs.git} ls-remote git://server/test"))

      token = server.succeed("""
        ${lib.getExe pkgs.curl} \
          -X POST \
          -H "Content-Type: application/json" \
          -d '{"loginname": "admin", "password": "admin_password"}' \
          http://gradient.local/api/v1/auth/basic/login \
          | ${lib.getExe pkgs.jq} -rj '.message'
      """)

      print(f"Got Token: {token}")

      server.succeed("${lib.getExe pkgs.gradient-cli} config Server http://gradient.local")
      server.succeed("${lib.getExe pkgs.gradient-cli} config AuthToken ACCESS_TOKEN".replace("ACCESS_TOKEN", token))

      server.succeed("${lib.getExe pkgs.gradient-cli} organization select org")
      server.succeed("${lib.getExe pkgs.gradient-cli} project select project")

      server.sleep(10)
      print(server.succeed("${lib.getExe pkgs.gradient-cli} project show"))

      # Wait for the server to detect the new commit (check cycle is 30 s).
      # Poll in short increments so we surface errors quickly instead of timing out.
      def check_journal_for_errors(since_seconds=45):
          j = server.succeed(f"journalctl -u gradient-server --no-pager --since='-{since_seconds}s' -n 200")
          if "panicked" in j or "SIGABRT" in j:
              raise Exception(f"Gradient server crashed:\n{j[-2000:]}")
          return j

      # First window: wait for repository detection
      detected = False
      for attempt in range(1, 7):
          server.sleep(15)
          j = check_journal_for_errors(since_seconds=attempt * 15 + 15)
          if "update needed" in j or "Force evaluation" in j or "triggered evaluation" in j or "Queued" in j:
              detected = True
              print(f"=== Repository update detected on attempt {attempt} ===")
              break
          if attempt == 6:
              raise Exception(f"Server did not detect repository change after 90 s:\n{j[-2000:]}")

      # Second window: wait for eval + build to complete (up to 300 s)
      # Use the API directly (curl) instead of the CLI to avoid non-zero exit
      # codes while the evaluation is still in progress.
      completed = False
      for attempt in range(1, 31):
          server.sleep(10)
          check_journal_for_errors(since_seconds=15)
          status = server.succeed("""
            ${lib.getExe pkgs.curl} -sf \
              -H "Authorization: Bearer ACCESS_TOKEN" \
              http://gradient.local/api/v1/projects/org/project \
              | ${lib.getExe pkgs.jq} -rj '.message.last_evaluation // empty'
          """.replace("ACCESS_TOKEN", token)).strip()
          if not status:
              if attempt % 3 == 0:
                  print(f"Still waiting for evaluation to start (attempt {attempt}/30)...")
              continue
          eval_status = server.succeed(f"""
            ${lib.getExe pkgs.curl} -sf \
              -H "Authorization: Bearer {token}" \
              http://gradient.local/api/v1/evals/{status} \
              | ${lib.getExe pkgs.jq} -rj '.message.status'
          """).strip()
          if eval_status == "Completed":
              completed = True
              print(f"=== Evaluation completed on attempt {attempt} ===")
              break
          elif eval_status == "Failed":
              j = server.succeed("journalctl -u gradient-server --no-pager --since='-300s' -n 200")
              bj = server.succeed("journalctl -u gradient-worker --no-pager --since='-300s' -n 200") if True else ""
              raise Exception(f"Evaluation failed:\nServer:\n{j[-2000:]}\nWorker:\n{bj[-2000:]}")
          if attempt % 3 == 0:
              # Print eval status, entry point count, and build status breakdown
              eval_detail = server.succeed(f"""
                ${lib.getExe pkgs.curl} -sf \
                  -H "Authorization: Bearer {token}" \
                  http://gradient.local/api/v1/evals/{status} \
                  | ${lib.getExe pkgs.jq} -c '.message | {{status, entry_points: (.entry_points | length)}}'
              """).strip()
              builds_summary = server.succeed(f"""
                ${lib.getExe pkgs.curl} -sf \
                  -H "Authorization: Bearer {token}" \
                  http://gradient.local/api/v1/evals/{status}/builds \
                  | ${lib.getExe pkgs.jq} -c '.message | {{total, by_status: ([.builds[].status] | group_by(.) | map({{key: .[0], value: length}}) | from_entries)}}'
              """).strip()
              print(f"Attempt {attempt}/30 — eval: {eval_detail}, builds: {builds_summary}")

      if not completed:
          j = server.succeed("journalctl -u gradient-server --no-pager --since='-300s' -n 200")
          raise Exception(f"Evaluation did not complete after 300 s:\n{j[-2000:]}")

      output = server.succeed("${lib.getExe pkgs.gradient-cli} project show")
      print(output)

      store_path_drv = ""
      in_building = False
      for line in output.split("\n"):
        if line.strip() == "===== Building =====":
          in_building = True
        elif line.strip() == "===== Log =====":
          break
        elif in_building and "hello" in line and line.strip().endswith(".drv"):
          drv = line.strip()
          store_path_drv = drv if drv.startswith("/nix/store/") else f"/nix/store/{drv}"
          break

      store_path = server.succeed(f"${lib.getExe pkgs.nix} path-info {store_path_drv}^out").strip()
      store_hash = store_path.split("-")[0].replace("/nix/store/", "")
      print(f"Detected store path: {store_path}")
      print(server.succeed(f"${lib.getExe pkgs.nix} path-info {store_path} --json"))

      print(server.succeed("su postgres -c 'psql -U postgres -d gradient -c \"SELECT * FROM organization_cache;\"'"))
      print(server.succeed("su postgres -c 'psql -U postgres -d gradient -c \"SELECT * FROM cache;\"'"))
      print(server.succeed("su postgres -c 'psql -U postgres -d gradient -c \"SELECT * FROM derivation_output_signature;\"'"))
      # TODO: Investigate why the folder is not created
      # print(server.succeed("${lib.getExe pkgs.tree} /var/lib/gradient/nars/"))
      print(client.succeed("${lib.getExe pkgs.curl} http://server/cache/main/nix-cache-info -i --fail"))
      print(client.succeed(f"${lib.getExe pkgs.curl} http://server/cache/main/{store_hash}.narinfo -i"))

      client.succeed(f"nix-store --delete {store_path} || true")
      client.fail(f"ls {store_path}")
      print(client.succeed(f"nix-store -vvv --realize {store_path}"))
      print(client.succeed(f"ls {store_path}"))
      '';
  });
}
