/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ self, pkgs, ... }: let
  # Bundles `pkgs.hello`'s full closure (`.drv` files, source tarballs, AND
  # directory outputs of every transitive build dep) at their canonical
  # /nix/store paths.  Used so the worker's BFS finds every derivation as
  # already substituted and never dispatches a source-fetch or compile job.
  # `skipDirectories = false` overrides the default flat-files-only mode
  # used by the Rust fixture loader.
  testStore = import ../../../scripts/store.nix {
    inherit pkgs;
    skipDirectories = false;
  };
in {
  value = pkgs.testers.runNixOSTest ({ pkgs, lib, ... }: {
    name = "gradient-cache";
    globalTimeout = 1800;

    defaults = {
      networking.firewall.enable = false;
      virtualisation = {
        cores = 4;
        memorySize = 2048;
        # Default 1024 MB diskSize is too small once the full hello build
        # closure (built outputs of stdenv/gcc/glibc/coreutils/…) is staged
        # into the VM via `additionalPaths` on the builder node.
        diskSize = 8192;
        writableStore = true;
      };
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
            reverseProxy.nginx.enable = true;
            configurePostgres = true;
            domain = "gradient.local";
            proto.public = true;
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
                  organizations = [ "org" ];
                  token_file = "/etc/gradient/secrets/worker_token";
                  created_by = "admin";
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

        # Ship hello's full build closure (`.drv` files, sources, and every
        # transitive output directory) into the worker VM's nix store, so
        # every derivation the worker walks is already substituted.  Without
        # this the worker would try to fetch tarballs from the internet -
        # which the test VM cannot reach - and every build would fail.
        virtualisation.additionalPaths = [ testStore ];

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
      import re

      # ── Helpers ───────────────────────────────────────────────────────────
      ANSI_RE = re.compile(r"\x1b\[[0-9;]*m")
      GIT     = "${lib.getExe pkgs.git}"
      CURL    = "${lib.getExe pkgs.curl}"
      JQ      = "${lib.getExe pkgs.jq}"
      NIX     = "${lib.getExe pkgs.nix}"
      CLI     = "${lib.getExe pkgs.gradient-cli}"
      API     = "http://gradient.local/api/v1"
      CACHE   = "http://server/cache/main"

      def banner(msg):
          """Loud step header, easy to grep in CI output."""
          print(f"\n=== {msg} ===")

      def api_get(token, path):
          """GET ``API/<path>``, return the parsed `.message` field as text."""
          return server.succeed(
              f'{CURL} -sf -H "Authorization: Bearer {token}" {API}/{path}'
          )

      def assert_no_server_panic(since_seconds=45):
          """Fail fast if gradient-server panicked since `since_seconds` ago."""
          j = server.succeed(
              f"journalctl -u gradient-server --no-pager --since='-{since_seconds}s' -n 200"
          )
          if "panicked" in j or "SIGABRT" in j:
              raise Exception(f"Gradient server crashed:\n{j[-2000:]}")
          return j

      start_all()

      # ── Phase 1: services come up and the worker authenticates ────────────
      banner("Phase 1: bring services up")
      server.wait_for_unit("gradient-server.service")
      server.sleep(5)
      builder.wait_for_unit("gradient-worker.service")

      builder.sleep(10)
      auth_logs = builder.succeed("journalctl -u gradient-worker --no-pager -n 100")
      assert "handshake successful" in auth_logs, \
          f"Worker did not authenticate successfully: {auth_logs[-500:]}"
      banner("Worker authenticated via state-managed registration")

      # ── Phase 2: seed the test git repository ─────────────────────────────
      banner("Phase 2: prepare test repository")
      server.succeed(f"{GIT} config --global --add safe.directory '*'")
      server.succeed(f"{GIT} config --global init.defaultBranch main")
      server.succeed(f"{GIT} config --global user.email 'nixos@localhost'")
      server.succeed(f"{GIT} config --global user.name 'NixOS test'")

      server.succeed(f"{GIT} init /var/lib/git/test")
      server.succeed("cp /var/lib/git/{,test/}flake.nix")
      server.succeed("cp /var/lib/git/{,test/}flake.lock")

      # The seed flake.{nix,lock} both pin nixpkgs to a `[nixpkgs]` placeholder;
      # rewrite them in-place so they point at the host nixpkgs path the test
      # was launched with (no internet in the VM).
      server.succeed("sed -i 's#\\[nixpkgs\\]#${self.inputs.nixpkgs}#g' /var/lib/git/test/flake.nix")
      server.succeed("sed -i 's#\\[nixpkgs\\]#${self.inputs.nixpkgs}#g' /var/lib/git/test/flake.lock")
      nixpkgs_hash = server.succeed(f"{NIX} hash path ${self.inputs.nixpkgs} --extra-experimental-features nix-command").strip()
      server.succeed(f"sed -i 's#\\[hash\\]#{nixpkgs_hash}#g' /var/lib/git/test/flake.lock")

      server.succeed(f"{GIT} -C /var/lib/git/test add flake.nix flake.lock")
      server.succeed(f"{GIT} -C /var/lib/git/test commit -m 'Initial commit'")
      server.succeed("chown git:git -R /var/lib/git/test")

      # Smoke-test that git-daemon serves the repo to anonymous clients.
      server.succeed(f"{GIT} clone git://localhost/test test")
      print(server.succeed(f"{GIT} ls-remote git://server/test"))

      # ── Phase 3: log in and configure the CLI ─────────────────────────────
      banner("Phase 3: authenticate and select project")
      login_body = '{"loginname": "admin", "password": "admin_password"}'
      token = server.succeed(
          f"{CURL} -X POST -H 'Content-Type: application/json' "
          f"-d '{login_body}' {API}/auth/basic/login | {JQ} -rj '.message'"
      ).strip()
      print(f"Got token: {token[:20]}…")

      server.succeed(f"{CLI} config Server http://gradient.local")
      server.succeed(f"{CLI} config AuthToken {token}")
      server.succeed(f"{CLI} organization select org")
      server.succeed(f"{CLI} project select project")

      # First `project show` is best-effort: the project may already have a
      # Queued evaluation, which the CLI exits 1 on. Use `execute` so a
      # transient non-zero exit doesn't abort the whole test.
      server.sleep(10)
      _, output = server.execute(f"{CLI} project show")
      print(output)

      # ── Phase 4: wait for the server to notice the new commit ─────────────
      # Project poll cycle is 30 s; we poll in 15 s slices so a panic shows
      # up instantly instead of after the full timeout.
      banner("Phase 4: wait for repository detection")
      detected = False
      for attempt in range(1, 7):
          server.sleep(15)
          j = assert_no_server_panic(since_seconds=attempt * 15 + 15)
          if any(needle in j for needle in (
              "update needed", "Force evaluation", "triggered evaluation", "Queued"
          )):
              detected = True
              banner(f"Repository update detected on attempt {attempt}")
              break
      if not detected:
          raise Exception(f"Server did not detect repository change after 90 s:\n{j[-2000:]}")

      # ── Phase 5: wait for the evaluation + builds to complete ─────────────
      # We hit the REST API directly (instead of the CLI) so a 404/empty body
      # while the eval is still being created doesn't crash us.
      banner("Phase 5: wait for evaluation to complete (up to 900 s)")
      eval_id = ""
      completed = False
      for attempt in range(1, 91):
          server.sleep(10)
          assert_no_server_panic(since_seconds=15)

          eval_id = server.succeed(
              f'{CURL} -sf -H "Authorization: Bearer {token}" '
              f'{API}/projects/org/project | {JQ} -rj ".message.last_evaluation // empty"'
          ).strip()
          if not eval_id:
              if attempt % 3 == 0:
                  print(f"  [{attempt:>2}/90] still waiting for evaluation to start…")
              continue

          eval_status = server.succeed(
              f'{CURL} -sf -H "Authorization: Bearer {token}" '
              f'{API}/evals/{eval_id} | {JQ} -rj ".message.status"'
          ).strip()

          if eval_status == "Completed":
              completed = True
              banner(f"Evaluation completed on attempt {attempt}")
              break

          if eval_status == "Failed":
              j  = server.succeed("journalctl -u gradient-server --no-pager --since='-300s' -n 200")
              bj = builder.succeed("journalctl -u gradient-worker --no-pager --since='-300s' -n 200")
              raise Exception(f"Evaluation failed:\nServer:\n{j[-2000:]}\nWorker:\n{bj[-2000:]}")

          if attempt % 3 == 0:
              eval_detail = server.succeed(
                  f'{CURL} -sf -H "Authorization: Bearer {token}" '
                  f'{API}/evals/{eval_id} | '
                  f'{JQ} -c ".message | {{status, entry_points: (.entry_points | length)}}"'
              ).strip()
              builds_summary = server.succeed(
                  f'{CURL} -sf -H "Authorization: Bearer {token}" '
                  f'{API}/evals/{eval_id}/builds | '
                  f'{JQ} -c ".message | {{total, by_status: ([.builds[].status] | group_by(.) | map({{key: .[0], value: length}}) | from_entries)}}"'
              ).strip()
              print(f"  [{attempt:>2}/90] eval={eval_detail} builds={builds_summary}")

      if not completed:
          j = server.succeed("journalctl -u gradient-server --no-pager --since='-900s' -n 200")
          raise Exception(f"Evaluation did not complete after 900 s:\n{j[-2000:]}")

      # ── Phase 6: extract hello's `.drv` from the CLI output ───────────────
      # The CLI's last step (log fetch) exits 1, so use `execute`. The
      # Project / Evaluation / Building sections we need are already printed
      # by then, and `colored` wraps build names in ANSI escapes which we
      # have to strip before pattern-matching.
      banner("Phase 6: extract hello's derivation path from `gradient project show`")
      _, output = server.execute(f"{CLI} project show")
      print(output)

      store_path_drv = ""
      in_building = False
      for line in output.split("\n"):
          clean = ANSI_RE.sub("", line).strip()
          if clean == "===== Building =====":
              in_building = True
              continue
          if clean == "===== Log =====":
              break
          if in_building and "hello" in clean and clean.endswith(".drv"):
              store_path_drv = clean if clean.startswith("/nix/store/") else f"/nix/store/{clean}"
              break
      assert store_path_drv, "could not find hello's .drv path in `gradient project show` output"

      # The `.drv` file is on the builder VM (its full closure was preseeded
      # via `additionalPaths`), not on the server, so resolve the output
      # path there.
      store_path = builder.succeed(
          f"{NIX} path-info {store_path_drv}^out --extra-experimental-features nix-command"
      ).strip()
      store_hash = store_path.split("-")[0].replace("/nix/store/", "")
      print(f"Built derivation: {store_path_drv}")
      print(f"Output path:      {store_path}")

      # ── Phase 7: verify the cache serves the narinfo ──────────────────────
      # `nix-cache-info` is unauthenticated and always available - a quick
      # smoke test that `/cache/main/*` is wired up.
      banner("Phase 7: cache serves nix-cache-info and the narinfo")
      print(client.succeed(f"{CURL} {CACHE}/nix-cache-info -i --fail"))

      # The sign-sweep loop ticks every 60 s; the freshly-built hello path
      # may not have a signature row yet, in which case the cache returns
      # 404. Poll up to 120 s for the signature to land.
      for sig_attempt in range(1, 25):
          rc, _ignored = client.execute(f"{CURL} -sf {CACHE}/{store_hash}.narinfo -o /dev/null")
          if rc == 0:
              banner(f"narinfo signed and served on poll {sig_attempt}")
              break
          client.sleep(5)
      print(client.succeed(f"{CURL} {CACHE}/{store_hash}.narinfo -i --fail"))

      # ── Phase 8: client substitutes hello straight from the cache ─────────
      # Drop the existing copy from the client store, then realize via
      # gradient cache only (the client's `substituters` is locked to
      # `http://server/cache/main`).
      banner("Phase 8: client realizes hello from gradient cache")
      client.succeed(f"nix-store --delete {store_path} || true")
      client.fail(f"ls {store_path}")
      print(client.succeed(f"nix-store -vvv --realize {store_path}"))
      print(client.succeed(f"ls {store_path}"))

      banner("Cache test PASSED")
      '';
  });
}
