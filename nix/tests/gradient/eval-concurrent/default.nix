/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

# Two projects pointing at the same repository evaluate concurrently on two
# eval-capable workers, so both stream the same derivation graph to the server
# at the same time. Asserts the content-addressed identity holds under that
# concurrency: shared `derivation`/`derivation_build` rows, both evals complete,
# and neither worker's IPC transport desyncs.
{ self, pkgs, ... }: let
  testStore = import ../../../scripts/store.nix {
    inherit pkgs;
    skipDirectories = false;
  };

  workerNode = workerId: { config, pkgs, lib, ... }: {
    imports = [ ../../../modules/gradient-worker.nix ];

    virtualisation.additionalPaths = [ testStore ];

    nix.settings = {
      trusted-users = [
        "root"
        "@wheel"
      ];

      max-jobs = lib.mkForce 8;
    };

    systemd.tmpfiles.rules = [
      "d /var/lib/gradient-worker 0755 gradient-worker gradient-worker"
      "f /var/lib/gradient-worker/worker-id 0644 gradient-worker gradient-worker - ${workerId}"
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
        eval = true;
        build = true;
      };
    };
  };
in {
  value = pkgs.testers.runNixOSTest ({ pkgs, lib, ... }: {
    name = "gradient-eval-concurrent";
    globalTimeout = 1800;

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
      server = { config, pkgs, lib, ... }: {
        imports = [
          ../../../modules/gradient.nix
        ];

        nix.settings.substituters = lib.mkForce [ ];
        environment = {
          variables.TEST_PKGS = [ self.inputs.nixpkgs ];
          systemPackages = with pkgs; [
            coreutils
            postgresql_18
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

              # Two projects on the SAME repository and wildcard: one active
              # evaluation each, both walking the identical derivation graph.
              projects = {
                left = {
                  organization = "org";
                  repository = "git://server/test";
                  wildcard = "packages.x86_64-linux.*";
                  created_by = "admin";
                  triggers = [
                    {
                      type = "polling";
                      config = { interval_secs = 10; };
                    }
                  ];
                };

                right = {
                  organization = "org";
                  repository = "git://server/test";
                  wildcard = "packages.x86_64-linux.*";
                  created_by = "admin";
                  triggers = [
                    {
                      type = "polling";
                      config = { interval_secs = 10; };
                    }
                  ];
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
                worker1 = {
                  worker_id = "a0000000-0000-0000-0000-000000000001";
                  organizations = [ "org" ];
                  token_file = "/etc/gradient/secrets/worker_token";
                  created_by = "admin";
                };

                worker2 = {
                  worker_id = "a0000000-0000-0000-0000-000000000002";
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

        environment.etc."gitconfig".text = ''
          [safe]
            directory = *
        '';

        systemd.tmpfiles.rules = [
          "d /var/lib/git 0755 git git"
          "L+ /var/lib/git/flake.nix 0755 git git - ${../cache/flake_repository.nix}"
          "L+ /var/lib/git/flake.lock 0755 git git - ${../cache/flake_repository.lock}"
        ];
      };

      worker1 = workerNode "a0000000-0000-0000-0000-000000000001";
      worker2 = workerNode "a0000000-0000-0000-0000-000000000002";
    };

    interactive.nodes = {
      server  = import ../../modules/debug-host.nix;
      worker1 = import ../../modules/debug-host.nix;
      worker2 = import ../../modules/debug-host.nix;
    };

    testScript = { nodes, ... }:
      ''
      GIT  = "${lib.getExe pkgs.git}"
      CURL = "${lib.getExe pkgs.curl}"
      JQ   = "${lib.getExe pkgs.jq}"
      NIX  = "${lib.getExe pkgs.nix}"
      PSQL = "${lib.getExe' pkgs.postgresql_18 "psql"}"
      API  = "http://gradient.local/api/v1"

      def banner(msg):
          print(f"\n=== {msg} ===")

      def psql(query):
          """Run one read-only query against the server DB, return trimmed stdout."""
          quoted = query.replace('"', '\\"')
          return server.succeed(
              f'{PSQL} -h 127.0.0.1 -U gradient -d gradient -tAc "{quoted}"'
          ).strip()

      def assert_no_server_panic(since_seconds=45):
          j = server.succeed(
              f"journalctl -u gradient-server --no-pager --since='-{since_seconds}s' -n 200"
          )
          if "panicked" in j or "SIGABRT" in j:
              raise Exception(f"Gradient server crashed:\n{j[-2000:]}")
          return j

      def eval_of(project, token):
          """The project's last evaluation id, or empty while none exists."""
          return server.succeed(
              f'{CURL} -sf -H "Authorization: Bearer {token}" '
              f'{API}/projects/org/{project} | {JQ} -rj ".message.last_evaluation // empty"'
          ).strip()

      def status_of(eval_id, token):
          return server.succeed(
              f'{CURL} -sf -H "Authorization: Bearer {token}" '
              f'{API}/evals/{eval_id} | {JQ} -rj ".message.status"'
          ).strip()

      start_all()

      # ── Phase 1: services up, both workers authenticated ──────────────────
      banner("Phase 1: bring services up")
      server.wait_for_unit("gradient-server.service")
      server.sleep(5)
      for w in (worker1, worker2):
          w.wait_for_unit("gradient-worker.service")
          w.wait_until_succeeds(
              "journalctl -u gradient-worker --no-pager | grep -q 'handshake successful'",
              timeout=180,
          )
      banner("Both workers authenticated")

      # ── Phase 2: seed the shared test repository ──────────────────────────
      banner("Phase 2: prepare test repository")
      server.succeed(f"{GIT} config --global --add safe.directory '*'")
      server.succeed(f"{GIT} config --global init.defaultBranch main")
      server.succeed(f"{GIT} config --global user.email 'nixos@localhost'")
      server.succeed(f"{GIT} config --global user.name 'NixOS test'")

      server.succeed(f"{GIT} init /var/lib/git/test")
      server.succeed("cp /var/lib/git/{,test/}flake.nix")
      server.succeed("cp /var/lib/git/{,test/}flake.lock")
      server.succeed("sed -i 's#\\[nixpkgs\\]#${self.inputs.nixpkgs}#g' /var/lib/git/test/flake.nix")
      server.succeed("sed -i 's#\\[nixpkgs\\]#${self.inputs.nixpkgs}#g' /var/lib/git/test/flake.lock")
      nixpkgs_hash = server.succeed(f"{NIX} hash path ${self.inputs.nixpkgs} --extra-experimental-features nix-command").strip()
      server.succeed(f"sed -i 's#\\[hash\\]#{nixpkgs_hash}#g' /var/lib/git/test/flake.lock")

      server.succeed(f"{GIT} -C /var/lib/git/test add flake.nix flake.lock")
      server.succeed(f"{GIT} -C /var/lib/git/test commit -m 'Initial commit'")
      server.succeed("chown git:git -R /var/lib/git/test")
      print(server.succeed(f"{GIT} ls-remote git://server/test"))

      # ── Phase 3: authenticate ─────────────────────────────────────────────
      banner("Phase 3: authenticate")
      login_body = '{"loginname": "admin", "password": "admin_password"}'
      token = server.succeed(
          f"{CURL} -X POST -H 'Content-Type: application/json' "
          f"-d '{login_body}' {API}/auth/basic/login | {JQ} -rj '.message'"
      ).strip()

      # ── Phase 4: both projects trigger an evaluation of the same commit ───
      # Both poll at 10 s, so the evals start within one cycle of each other
      # and stream the same derivation graph to the server concurrently.
      banner("Phase 4: wait for both evaluations to start")
      evals = {"left": "", "right": ""}
      for attempt in range(1, 31):
          server.sleep(10)
          assert_no_server_panic(since_seconds=15)
          for p in evals:
              if not evals[p]:
                  evals[p] = eval_of(p, token)
          if all(evals.values()):
              banner(f"Both evaluations started on attempt {attempt}: {evals}")
              break
      assert all(evals.values()), f"evaluations did not start after 300 s: {evals}"

      # ── Phase 5: both evaluations complete ────────────────────────────────
      banner("Phase 5: wait for both evaluations to complete (up to 900 s)")
      done = {p: False for p in evals}
      for attempt in range(1, 91):
          server.sleep(10)
          assert_no_server_panic(since_seconds=15)
          for p, e in evals.items():
              if done[p]:
                  continue
              s = status_of(e, token)
              if s == "Completed":
                  done[p] = True
                  banner(f"Evaluation of {p} completed on attempt {attempt}")
              elif s == "Failed":
                  j  = server.succeed("journalctl -u gradient-server --no-pager --since='-300s' -n 200")
                  w1 = worker1.succeed("journalctl -u gradient-worker --no-pager --since='-300s' -n 100")
                  w2 = worker2.succeed("journalctl -u gradient-worker --no-pager --since='-300s' -n 100")
                  raise Exception(
                      f"Evaluation of {p} failed:\nServer:\n{j[-2000:]}\n"
                      f"Worker1:\n{w1[-1500:]}\nWorker2:\n{w2[-1500:]}"
                  )
          if all(done.values()):
              break
      assert all(done.values()), f"evaluations did not complete after 900 s: {done}"

      # Show how the two eval windows and their workers interleaved.
      print(psql(
          "SELECT id, eval_flake_started_at, building_started_at, finished_at "
          f"FROM evaluation WHERE id IN ('{evals['left']}', '{evals['right']}')"
      ))

      # ── Phase 6: one shared content-addressed graph ───────────────────────
      # The second eval may legitimately report FEWER derivations (the BFS
      # prunes subtrees whose outputs the first eval already cached), so the
      # invariant is identity, not set equality: every drv reported by both
      # evals resolves to the same `derivation` row and the same
      # `derivation_build` anchor, and no `(hash, name)` exists twice.
      banner("Phase 6: assert shared derivation identity")
      e_left, e_right = evals["left"], evals["right"]
      n_left  = int(psql(f"SELECT count(*) FROM build_job WHERE evaluation = '{e_left}'"))
      n_right = int(psql(f"SELECT count(*) FROM build_job WHERE evaluation = '{e_right}'"))
      overlap = int(psql(
          f"SELECT count(*) FROM (SELECT derivation FROM build_job WHERE evaluation = '{e_left}' "
          f"INTERSECT SELECT derivation FROM build_job WHERE evaluation = '{e_right}') x"
      ))
      print(f"build_jobs: left={n_left} right={n_right} shared drvs={overlap}")
      assert n_left > 0 and n_right > 0, "both evals must report a graph"
      assert overlap > 0, "the two evals of one commit must share derivations"

      dup_drvs = int(psql(
          "SELECT count(*) FROM (SELECT hash, name FROM derivation "
          "GROUP BY hash, name HAVING count(*) > 1) d"
      ))
      assert dup_drvs == 0, f"{dup_drvs} duplicated (hash, name) derivation rows"

      dup_anchors = int(psql(
          "SELECT count(*) FROM (SELECT derivation FROM derivation_build "
          "GROUP BY derivation HAVING count(*) > 1) d"
      ))
      assert dup_anchors == 0, f"{dup_anchors} derivations with duplicated build anchors"

      # Both evals' root must be the one hello derivation row.
      hello_ids = int(psql(
          "SELECT count(DISTINCT bj.derivation) FROM build_job bj "
          "JOIN derivation d ON d.id = bj.derivation "
          f"WHERE bj.evaluation IN ('{e_left}', '{e_right}') AND d.name LIKE 'hello-%'"
      ))
      assert hello_ids == 1, f"expected one shared hello derivation, got {hello_ids}"

      # ── Phase 7: no eval-worker IPC desync on either worker ───────────────
      # Regression guard for the pooled-transport lockstep: a stale frame
      # served to a later request logs "unexpected response to".
      banner("Phase 7: no IPC desync in worker logs")
      for w in (worker1, worker2):
          w.fail("journalctl -u gradient-worker --no-pager | grep -q 'unexpected response to'")

      assert_no_server_panic()
      banner("Concurrent eval test PASSED")
      '';
  });
}
