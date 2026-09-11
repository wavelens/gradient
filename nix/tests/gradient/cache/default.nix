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

  builderNode = workerId: { config, pkgs, lib, ... }: {
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
      settings.buildMetrics = true;
      capabilities = {
        eval  = true;
        build = true;
      };
    };
  };
in {
  value = pkgs.testers.runNixOSTest ({ pkgs, lib, ... }: {
    name = "gradient-cache";
    # Phases 10e and 10f add a second evaluation of the same commit plus a
    # two-session lock handshake to what was already a full build-and-cache run.
    globalTimeout = 2400;

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
                  superuser = true;
                };
              };

              projects = {
                project = {
                  private_key_file = "/etc/gradient/secrets/corp_ssh_key";
                  created_by = "admin";
                };
              };

              tasks = {
                task = {
                  project = "project";
                  repository = "git://server/test";
                  created_by = "admin";
                  triggers = [
                    {
                      type = "polling";
                      config = { interval_secs = 10; };
                    }
                  ];
                };

                task2 = {
                  project = "project";
                  repository = "git://server/test";
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
                  projects = [ "project" ];
                  public = true;
                  created_by = "admin";
                };
              };

              workers = {
                builder = {
                  worker_id = "a0000000-0000-0000-0000-000000000001";
                  projects = [ "project" ];
                  token_file = "/etc/gradient/secrets/worker_token";
                  created_by = "admin";
                };

                builder2 = {
                  worker_id = "a0000000-0000-0000-0000-000000000002";
                  projects = [ "project" ];
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
              # Phase 10d bills the server's statements; the extension only
              # exposes the view, the accounting needs this preload, and its
              # deltas need every entry to survive both samples.
              shared_preload_libraries = "pg_stat_statements";
              "pg_stat_statements.max" = 10000;
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

      builder  = builderNode "a0000000-0000-0000-0000-000000000001";
      builder2 = builderNode "a0000000-0000-0000-0000-000000000002";

      client = { config, pkgs, lib, ... }: {
        environment.variables.TEST_PKGS = [ self.inputs.nixpkgs ];
        nix.settings = {
          substituters = lib.mkForce [ "http://server/cache/main" ];
          trusted-public-keys = lib.mkForce [ "gradient.local-main:bw27zKszGUvnq/wRPLnG8TUhuSmfAdBCzuEyWpfJmZc=" ];
        };
      };
    };

    interactive.nodes = {
      server   = import ../../modules/debug-host.nix;
      builder  = import ../../modules/debug-host.nix;
      builder2 = import ../../modules/debug-host.nix;
      client   = import ../../modules/debug-host.nix;
    };

    testScript = { nodes, ... }:
      ''
      # ── Helpers ───────────────────────────────────────────────────────────
      import json
      import time

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

      def sql(query):
          server.succeed(f"cat > /tmp/q.sql <<'EOF'\n{query}\nEOF")
          return server.succeed("su postgres -c 'psql -d gradient -At -f /tmp/q.sql'").strip()

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

      # Phase 10f's two-session handshake. Parameterless on purpose: it reads
      # LR_DRV and LR_OUT from the environment so the body needs no substitution
      # and stays a plain string. Each `wait_state` is bounded and dumps both
      # session logs plus pg_stat_activity before failing, so the phase can never
      # hang CI, and `cleanup` runs on every exit path - a leaked backend would
      # hold row locks into phase 11.
      LOCK_RACE_SH = """
      set -u
      D=/tmp/lockrace
      APID=""
      BPID=""

      cleanup() {
        exec 3>&-
        exec 4>&-
        if [ -n "$APID" ]; then kill $APID 2>/dev/null || true; fi
        if [ -n "$BPID" ]; then kill $BPID 2>/dev/null || true; fi
        mkdir -p $D
        printf '%s\\n' "SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE application_name LIKE 'lockrace_%';" > $D/kill.sql
        su postgres -c "psql -d gradient -At -q -f $D/kill.sql" >/dev/null 2>&1 || true
        rm -rf $D
      }
      trap cleanup EXIT INT TERM

      q() {
        printf '%s\\n' "$1" > $D/q.sql
        su postgres -c "psql -d gradient -At -q -f $D/q.sql"
      }

      wait_state() {
        i=0
        while [ $i -lt 60 ]; do
          if [ "$(q "SELECT count(*) FROM pg_stat_activity WHERE $1")" = "1" ]; then
            return 0
          fi
          i=$((i + 1))
          sleep 1
        done
        echo "LOCKRACE TIMEOUT: $2"
        echo "--- retire session ---"
        cat $D/a.out || true
        echo "--- recount session ---"
        cat $D/b.out || true
        q "SELECT pid, application_name, state, wait_event_type, wait_event, left(query, 90) FROM pg_stat_activity WHERE datname = 'gradient' ORDER BY pid"
        exit 1
      }

      send_a() { printf '%s\\n' "$1" >&3; }
      send_b() { printf '%s\\n' "$1" >&4; }

      rm -rf $D
      mkdir -p $D
      mkfifo $D/a.in
      mkfifo $D/b.in
      su postgres -c "psql -d gradient -At" < $D/a.in > $D/a.out 2>&1 &
      APID=$!
      su postgres -c "psql -d gradient -At" < $D/b.in > $D/b.out 2>&1 &
      BPID=$!
      exec 3> $D/a.in
      exec 4> $D/b.in

      recount() {
        printf '%s\\n' "UPDATE derivation_build db SET fetchable = x.f FROM (SELECT p.derivation, p.fetchable AS old, (p.substitutable OR (p.status IN (3, 7) AND EXISTS (SELECT 1 FROM derivation_output o2 WHERE o2.derivation = p.derivation) AND NOT EXISTS (SELECT 1 FROM derivation_output o LEFT JOIN cached_path cp ON cp.hash = o.hash WHERE o.derivation = p.derivation AND NOT (cp.file_hash IS NOT NULL AND cp.missing_references = 0)))) AS f FROM derivation_build p WHERE p.derivation = ANY(ARRAY['$1']::uuid[])) x WHERE db.derivation = x.derivation AND db.fetchable = x.old AND x.old <> x.f;"
      }

      # The retire's shape: its opening hash-ordered lock pass, the delete, then the
      # anchor lock its readiness half takes. The `fetchable` mark is deliberately
      # absent - the anchor is already stored not-fetchable, which is the drift the
      # recount is meant to correct and the reason a stale one writes true.
      send_a "SET application_name = 'lockrace_a';"
      send_a "BEGIN;"
      send_a "SELECT 1 FROM cached_path WHERE hash = ANY(ARRAY['$LR_OUT']) ORDER BY hash FOR UPDATE;"
      send_a "DELETE FROM cached_path WHERE hash = ANY(ARRAY['$LR_OUT']);"
      send_a "SELECT 1 FROM derivation_build WHERE derivation = ANY(ARRAY['$LR_DRV']::uuid[]) ORDER BY derivation FOR UPDATE;"
      wait_state "application_name = 'lockrace_a' AND state = 'idle in transaction'" "the retire session never reached its held state"

      send_b "SET application_name = 'lockrace_b';"
      send_b "BEGIN;"
      send_b "SELECT 1 FROM derivation_build WHERE derivation = ANY(ARRAY['$LR_DRV']::uuid[]) ORDER BY derivation FOR UPDATE;"
      wait_state "application_name = 'lockrace_b' AND wait_event_type = 'Lock'" "the recount session never blocked on the anchor lock"
      echo "lockrace: the recount is blocked on the retire"

      send_a "COMMIT;"
      wait_state "application_name = 'lockrace_a' AND state = 'idle'" "the retire session never committed"

      send_b "$(recount "$LR_DRV")"
      send_b "COMMIT;"
      wait_state "application_name = 'lockrace_b' AND state = 'idle'" "the recount session never committed"

      if grep -q ERROR $D/a.out $D/b.out; then
        echo "LOCKRACE: a session reported an error"
        cat $D/a.out
        cat $D/b.out
        exit 1
      fi

      # The recount MUST have run and written nothing: under the lock its snapshot
      # sees the deleted output, so the recomputed value equals the stored false and
      # the compare-and-swap has nothing to write. A missing tag here would let the
      # phase pass on a recount that never executed.
      if ! grep -q "UPDATE 0" $D/b.out; then
        echo "LOCKRACE: the recount did not run, or wrote a value the retire made stale"
        cat $D/b.out
        exit 1
      fi
      echo "lockrace: both sessions committed and the recount was a no-op"

      # The same interleaving with the lock removed, on its own row. Without a
      # preceding FOR UPDATE the recount's own statement takes the snapshot and THEN
      # waits, so it reads the output as whole, blocks, and EvalPlanQual re-checks
      # only the target row's own column before the stale `true` lands on an anchor
      # whose output is gone. Measured both ways on Postgres 18 before it was written
      # here: locked writes nothing, unlocked writes true. The CONTRAST is the
      # assertion. If the two arms ever agree, the lock stopped being what makes the
      # recount correct and a human needs to know that.
      send_a "BEGIN;"
      send_a "SELECT 1 FROM cached_path WHERE hash = ANY(ARRAY['$LR_OUT2']) ORDER BY hash FOR UPDATE;"
      send_a "DELETE FROM cached_path WHERE hash = ANY(ARRAY['$LR_OUT2']);"
      send_a "SELECT 1 FROM derivation_build WHERE derivation = ANY(ARRAY['$LR_DRV2']::uuid[]) ORDER BY derivation FOR UPDATE;"
      wait_state "application_name = 'lockrace_a' AND state = 'idle in transaction'" "the retire session never held its second row"

      send_b "BEGIN;"
      send_b "$(recount "$LR_DRV2")"
      wait_state "application_name = 'lockrace_b' AND wait_event_type = 'Lock'" "the unlocked recount never blocked on the retire"
      send_a "COMMIT;"
      wait_state "application_name = 'lockrace_b' AND state = 'idle in transaction'" "the unlocked recount never resumed after the retire committed"
      send_b "COMMIT;"
      wait_state "application_name = 'lockrace_b' AND state = 'idle'" "the unlocked recount session never committed"

      if grep -q ERROR $D/a.out $D/b.out; then
        echo "LOCKRACE: a session reported an error in the unlocked arm"
        cat $D/a.out
        cat $D/b.out
        exit 1
      fi
      echo "lockrace: the unlocked arm committed; the driver checks what it wrote"
      """

      start_all()

      # ── Phase 1: services come up and the worker authenticates ────────────
      banner("Phase 1: bring services up")
      server.wait_for_unit("gradient-server.service")
      server.sleep(5)
      builder.wait_for_unit("gradient-worker.service")
      builder2.wait_for_unit("gradient-worker.service")

      # On a fresh DB the server applies its full migration set before binding
      # :3000, so the worker (capped exponential reconnect backoff) can take well
      # over a minute to authenticate. Wait for the handshake instead of asserting
      # once after a fixed sleep, which races the slow cold start.
      builder.wait_until_succeeds(
          "journalctl -u gradient-worker --no-pager | grep -q 'handshake successful'",
          timeout=180,
      )
      builder2.wait_until_succeeds(
          "journalctl -u gradient-worker --no-pager | grep -q 'handshake successful'",
          timeout=180,
      )
      banner("Both workers authenticated via state-managed registration")

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
      banner("Phase 3: authenticate and select task")
      login_body = '{"loginname": "admin", "password": "admin_password"}'
      token = server.succeed(
          f"{CURL} -X POST -H 'Content-Type: application/json' "
          f"-d '{login_body}' {API}/auth/basic/login | {JQ} -rj '.message'"
      ).strip()
      print(f"Got token: {token[:20]}…")

      server.succeed(f"{CLI} config Server http://gradient.local")
      server.succeed(f"{CLI} config AuthToken {token}")
      server.succeed(f"{CLI} project select project")
      server.succeed(f"{CLI} task select task")

      # First `task show` is best-effort: the task may already have a
      # Queued evaluation, which the CLI exits 1 on. Use `execute` so a
      # transient non-zero exit doesn't abort the whole test.
      server.sleep(10)
      _, output = server.execute(f"{CLI} task show")
      print(output)

      # ── Phase 4: wait for the server to notice the new commit ─────────────
      # Task poll cycle is configured to 10 s in the state above; we poll
      # in 15 s slices so a panic shows up instantly instead of after the
      # full timeout.
      banner("Phase 4: wait for repository detection")
      detected = False
      for attempt in range(1, 7):
          server.sleep(15)
          j = assert_no_server_panic(since_seconds=attempt * 15 + 15)
          if any(needle in j for needle in (
              "update needed", "Force evaluation", "trigger created evaluation", "Queued"
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

          print("  DEBUG get_task: " + server.succeed(
              f'{CURL} -s -w " [HTTP %{{http_code}}]" -H "Authorization: Bearer {token}" {API}/tasks/project/task'
          ))
          eval_id = server.succeed(
              f'{CURL} -sf -H "Authorization: Bearer {token}" '
              f'{API}/tasks/project/task | {JQ} -rj ".message.last_evaluation // empty"'
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
          # A stall is an anchor that never went terminal, so the anchor states are the
          # diagnosis and the journal is the supporting evidence. The old dump was the
          # last 2000 characters of a DEBUG journal, which is six lines of HTTP request
          # logging and names nothing.
          anchors = sql(
              f"SELECT db.status::text || ' fetchable=' || db.fetchable::int::text"
              f"  || ' unready_deps=' || db.unready_deps || ' count=' || count(*)::text"
              f" FROM derivation_build db"
              f" JOIN build_job bj ON bj.derivation_build = db.id"
              f" WHERE bj.evaluation = '{eval_id}'"
              f" GROUP BY db.status, db.fetchable, db.unready_deps ORDER BY 1;"
          )
          j = server.succeed(
              "journalctl -u gradient-server --no-pager --since='-900s' -n 4000"
              " | grep -vE 'gradient_web: (request started|response generated"
              "|sending chunk|stream closed)'"
              " | tail -n 80"
          )
          raise Exception(
              f"Evaluation did not complete after 900 s.\n"
              f"anchors of this evaluation:\n{anchors}\n"
              f"server log, HTTP request noise removed:\n{j}"
          )

      # ── Phase 5b: the worker committed a non-empty eval cache to disk ─────
      # Regression guard (#386): nix commits the eval-cache AttrDb only on
      # EvalState teardown / WAL checkpoint, but the worker reads & pushes the
      # `<fp>.sqlite` while still alive, so the file never grew past the 4096-
      # byte SQLite header and every flake re-evaluated cold. A committed cache
      # is >=12 KB (schema) and larger once attributes are memoised.
      banner("Phase 5b: worker eval-cache is committed (non-empty)")
      eval_cache_dir = "/var/lib/gradient-worker/eval-cache/eval-cache-v6"
      builder.succeed(f"test -d {eval_cache_dir}")
      sizes = builder.succeed(
          f"find {eval_cache_dir} -name '*.sqlite' -printf '%s %p\\n' | sort -rn"
      ).strip()
      print(sizes or "(no .sqlite files)")
      biggest = int(sizes.splitlines()[0].split()[0]) if sizes else 0
      assert biggest > 4096, (
          f"eval-cache never committed: largest .sqlite is {biggest} bytes "
          f"(4096 = empty SQLite header).\n{sizes}"
      )


      # ── Phase 5c: two workers merged one graph, and nothing raced ─────────
      # `task2` polls the same repository, so its evaluation of the same commit
      # ran on the second worker while the first was still streaming batches.
      # Every write went through the graph actor: one derivation row per hash,
      # identical build sets, no anchor left without its edges, and no pool
      # exhaustion or dropped call in the server log.
      banner("Phase 5c: a concurrent evaluation of the same commit merged cleanly")
      eval2_id = ""
      for attempt in range(1, 61):
          eval2_id = server.succeed(
              f'{CURL} -sf -H "Authorization: Bearer {token}" '
              f'{API}/tasks/project/task2 | {JQ} -rj ".message.last_evaluation // empty"'
          ).strip()
          if eval2_id:
              status2 = server.succeed(
                  f'{CURL} -sf -H "Authorization: Bearer {token}" '
                  f'{API}/evals/{eval2_id} | {JQ} -rj ".message.status"'
              ).strip()
              if status2 == "Completed":
                  break
              if status2 == "Failed":
                  j = server.succeed("journalctl -u gradient-server --no-pager --since='-600s' -n 300")
                  raise Exception(f"second evaluation failed:\n{j[-3000:]}")
          server.sleep(10)
      else:
          raise Exception("second evaluation did not complete after 600 s")
      assert eval2_id != eval_id, "task2 must have its own evaluation"

      builds1 = int(sql(f"SELECT count(*) FROM build_job WHERE evaluation = '{eval_id}';"))
      builds2 = int(sql(f"SELECT count(*) FROM build_job WHERE evaluation = '{eval2_id}';"))
      print(f"build jobs: eval1={builds1} eval2={builds2}")
      assert builds1 > 0 and builds1 == builds2, "both evaluations record the same number of builds"
      only_in_one = int(sql(
          f"SELECT count(*) FROM ("
          f"  SELECT derivation FROM build_job WHERE evaluation = '{eval_id}'"
          f"  EXCEPT SELECT derivation FROM build_job WHERE evaluation = '{eval2_id}'"
          f") x;"
      ))
      assert only_in_one == 0, f"{only_in_one} derivations are in the first evaluation only"
      duplicates = int(sql("SELECT count(*) - count(DISTINCT hash) FROM derivation;"))
      assert duplicates == 0, f"{duplicates} duplicate derivation rows"
      unwalked = int(sql(
          f"SELECT count(*) FROM build_job bj JOIN derivation d ON d.id = bj.derivation "
          f"WHERE bj.evaluation IN ('{eval_id}', '{eval2_id}') AND NOT d.walked;"
      ))
      assert unwalked == 0, f"{unwalked} derivations of the two evaluations are stubs"
      edges = int(sql("SELECT count(*) FROM derivation_dependency;"))
      assert edges > 0, "the graph recorded no dependency edge at all"
      unwalked_deps = int(sql(
          "SELECT count(*) FROM derivation_dependency e JOIN derivation d ON d.id = e.dependency "
          "WHERE NOT d.walked;"
      ))
      assert unwalked_deps == 0, f"{unwalked_deps} of {edges} dependency edges point at a stub"
      j = server.succeed("journalctl -u gradient-server --no-pager")
      for needle in ("pool timed out", "graph call timed out", "graph actor unreachable",
                     "ingest transaction failed", "dropped as stale"):
          assert needle not in j, f"server log contains {needle!r}"

      # ── Phase 5b: the graph walks stay fenced and agree with the old shape ─
      # The recursive walks are generated in `graph_sql.rs` as a LATERAL probe
      # behind an `OFFSET 0` fence. Without the fence Postgres believes the
      # working table is ten times the seed and merge-joins the whole edge
      # table once per iteration, which cost 5.3 s on a 44k-node production
      # closure against 1.0 s fenced. Nothing in the Rust type system notices
      # if the fence is dropped, so assert the plan here.
      banner("Phase 5b: graph walk shape, plans and closure counters")

      have_reverse_index = int(sql(
          "SELECT count(*) FROM pg_indexes "
          "WHERE indexname = 'idx-derivation_dependency-reverse-pair';"
      ))
      assert have_reverse_index == 1, "the reverse walk lost its covering index"
      have_referrer_index = int(sql(
          "SELECT count(*) FROM pg_indexes "
          "WHERE indexname = 'idx-cached_path_reference-referrer-hash';"
      ))
      assert have_referrer_index == 1, "the reference walk lost its covering index"
      leftover_keys = int(sql(
          "SELECT count(*) FROM information_schema.columns "
          "WHERE column_name = 'id' AND table_name IN "
          "('derivation_dependency', 'derivation_closure', 'cached_path_reference');"
      ))
      assert leftover_keys == 0, f"{leftover_keys} junction tables kept a surrogate key"

      # The fenced walk and the plain join must select the same node set. This is
      # the only place the rewrite is checked against a real graph.
      for direction, fenced_step, plain_step in [
          ("dependencies",
           "SELECT e.dependency AS next FROM derivation_dependency e WHERE e.derivation = c.derivation",
           "SELECT e.dependency FROM derivation_dependency e JOIN plain c ON e.derivation = c.derivation"),
          ("dependents",
           "SELECT e.derivation AS next FROM derivation_dependency e WHERE e.dependency = c.derivation",
           "SELECT e.derivation FROM derivation_dependency e JOIN plain c ON e.dependency = c.derivation"),
      ]:
          seed = f"SELECT bj.derivation FROM build_job bj WHERE bj.evaluation = '{eval_id}'"
          disagree = int(sql(
              f"WITH RECURSIVE fenced(derivation) AS ({seed} UNION "
              f"  SELECT s.next FROM fenced c, LATERAL ({fenced_step} OFFSET 0) s), "
              f"plain(derivation) AS ({seed} UNION {plain_step}) "
              f"SELECT (SELECT count(*) FROM (SELECT derivation FROM fenced "
              f"        EXCEPT SELECT derivation FROM plain) a) "
              f"     + (SELECT count(*) FROM (SELECT derivation FROM plain "
              f"        EXCEPT SELECT derivation FROM fenced) b);"
          ))
          assert disagree == 0, f"the fenced {direction} walk disagrees on {disagree} nodes"

          # A lateral correlation can only be executed as a nested loop, so the
          # fence holding is exactly "no merge join survived in the plan".
          plan = sql(
              f"EXPLAIN WITH RECURSIVE fenced(derivation) AS ({seed} UNION "
              f"  SELECT s.next FROM fenced c, LATERAL ({fenced_step} OFFSET 0) s) "
              f"SELECT count(*) FROM fenced;"
          )
          assert "Nested Loop" in plan, f"the {direction} walk lost its nested loop:\n{plan}"
          assert "Merge Join" not in plan, f"the {direction} walk merge-joins again:\n{plan}"

      # The maintained histogram is what the task page reads; recomputing it from
      # the materialised closure must give the same totals.
      drift = int(sql(
          f"WITH stored AS ("
          f"  SELECT ep.id, coalesce(sum(c.count), 0) AS total FROM entry_point ep "
          f"  LEFT JOIN entry_point_dep_count c ON c.entry_point = ep.id "
          f"  WHERE ep.evaluation = '{eval_id}' GROUP BY ep.id), "
          f"live AS ("
          f"  SELECT ep.id, count(*) AS total FROM entry_point ep "
          f"  JOIN derivation_closure dc ON dc.root_derivation = ep.derivation "
          f"  JOIN derivation_build b ON b.derivation = dc.dep_derivation "
          f"  WHERE ep.evaluation = '{eval_id}' GROUP BY ep.id) "
          f"SELECT count(*) FROM live l JOIN stored s ON s.id = l.id "
          f"WHERE l.total <> s.total;"
      ))
      assert drift == 0, f"{drift} entry points disagree with their materialised closure"

      # ── Phase 6: extract hello's `.drv` from the eval's build list ────────
      # We hit `/evals/{id}/builds` directly with the eval_id already pinned
      # by Phase 5; screen-scraping `gradient task show` is too brittle
      # (polling can rotate `last_evaluations[0]` to a fresh Queued eval
      # between phases, and the CLI then errors on the eval-detail fetch
      # before reaching the Building section).
      banner("Phase 6: extract hello's derivation path from /evals/{id}/builds")
      store_path_drv = server.succeed(
          f'{CURL} -sf -H "Authorization: Bearer {token}" '
          f'{API}/evals/{eval_id}/builds | '
          f'{JQ} -r \'.message.builds[] | select(.name | test("hello[^/]*\\\\.drv$")) | .name\' | head -n1'
      ).strip()
      assert store_path_drv, f"could not find hello's .drv in eval {eval_id}'s builds"
      if not store_path_drv.startswith("/nix/store/"):
          store_path_drv = f"/nix/store/{store_path_drv}"

      # The `.drv` file is on the builder VM (its full closure was preseeded
      # via `additionalPaths`), not on the server, so resolve the output
      # path there.
      store_path = builder.succeed(
          f"{NIX} path-info {store_path_drv}^out --extra-experimental-features nix-command"
      ).strip()
      store_hash = store_path.split("-")[0].replace("/nix/store/", "")
      print(f"Built derivation: {store_path_drv}")
      print(f"Output path:      {store_path}")

      # Every edge a batch declares lands in the batch's own transaction, so the
      # graph must record exactly the input drvs the `.drv` itself declares.
      drv_hash = store_path_drv.split("/")[-1].split("-")[0]
      declared = int(builder.succeed(
          f"{NIX} derivation show {store_path_drv} --extra-experimental-features nix-command "
          f"| {JQ} '[.derivations[] | (.inputDrvs // .inputs.drvs // {{}}) | length] | add'"
      ).strip())
      recorded = int(sql(
          f"SELECT count(*) FROM derivation_dependency e JOIN derivation d ON d.id = e.derivation "
          f"WHERE d.hash = '{drv_hash}';"
      ))
      assert declared > 0, f"{store_path_drv} declares no input drv; the check would pass on nothing"
      assert declared == recorded, f"hello declares {declared} input drvs, the graph records {recorded}"

      # ── Phase 7: verify the cache serves the narinfo ──────────────────────
      # `nix-cache-info` is unauthenticated and always available - a quick
      # smoke test that `/cache/main/*` is wired up.
      banner("Phase 7: cache serves nix-cache-info and the narinfo")
      print(client.succeed(f"{CURL} {CACHE}/nix-cache-info -i --fail"))

      # A freshly cached path is signed in place on upload, but the commit runs
      # on a detached task, so the signature may lag the build's completion by a
      # moment (or fall back to the periodic sweep). Poll up to 120 s for it.
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

      # ── Phase 9: `gradient cache upload` compresses + signs (regression #509) ─
      # Add a unique leaf path to the server store and upload it with the CLI:
      # the default uploads the runtime closure, zstd-compressed, and the server
      # signs it in place. The client (substituters locked to the gradient cache)
      # then realizes it. A raw/uncompressed NAR fails the client's zstd import
      # ("Unknown frame descriptor"); an unsigned narinfo fails its signature
      # check. Both fixes must hold for realize to succeed.
      banner("Phase 9: gradient cache upload compresses + signs (#509)")
      server.succeed("echo gradient-upload-regression-509 > /tmp/upload-probe.txt")
      upload_path = server.succeed("nix-store --add /tmp/upload-probe.txt").strip()
      upload_hash = upload_path.split("-")[0].replace("/nix/store/", "")
      print(f"Uploading probe path: {upload_path}")
      print(server.succeed(f"{CLI} cache upload main {upload_path}"))

      client.wait_until_succeeds(
          f"{CURL} -sf {CACHE}/{upload_hash}.narinfo -o /dev/null", timeout=60
      )
      print(client.succeed(f"{CURL} {CACHE}/{upload_hash}.narinfo -i --fail"))

      client.fail(f"ls {upload_path}")
      print(client.succeed(f"nix-store -vvv --realize {upload_path}"))
      print(client.succeed(f"ls {upload_path}"))

      # ── Phase 10: debuginfo index + 404 on unknown keys (#563) ────────────
      # A `separateDebugInfo` output carries `lib/debug/.build-id/<xx>/<yy>.debug`.
      # Uploading one must make `debuginfo/<build-id>` resolve to that NAR member,
      # in the same JSON shape nix writes under `index-debug-info=true`, and every
      # key we do not serve must be a 404: nixseparatedebuginfod aborts the whole
      # lookup on any other status.
      banner("Phase 10: debuginfo index and 404 on unknown keys (#563)")
      build_id = "7dbeaca53fbc9a489b633871093c37dae3857a37"
      member = f"lib/debug/.build-id/{build_id[:2]}/{build_id[2:]}.debug"
      server.succeed(f"mkdir -p /tmp/probe-debug/lib/debug/.build-id/{build_id[:2]}")
      server.succeed(f"echo gradient-debuginfo-563 > /tmp/probe-debug/{member}")
      debug_path = server.succeed("nix-store --add /tmp/probe-debug").strip()
      debug_hash = debug_path.split("-")[0].replace("/nix/store/", "")
      print(f"Uploading debug output: {debug_path}")
      print(server.succeed(f"{CLI} cache upload main {debug_path}"))

      client.wait_until_succeeds(
          f"{CURL} -sf {CACHE}/{debug_hash}.narinfo -o /dev/null", timeout=60
      )

      # The build-id walk runs detached from the upload commit, so poll for it.
      client.wait_until_succeeds(
          f"{CURL} -sf {CACHE}/debuginfo/{build_id} -o /dev/null", timeout=60
      )
      redirect = client.succeed(f"{CURL} -sf {CACHE}/debuginfo/{build_id}")
      print(redirect)
      parsed = json.loads(redirect)
      assert parsed["member"] == member, redirect
      assert parsed["archive"].startswith("../nar/"), redirect
      assert parsed["archive"].endswith(".nar.zst"), redirect

      # `nix copy` writes the key with the `.debug` suffix still attached; both
      # spellings must resolve to the same document.
      assert client.succeed(f"{CURL} -sf {CACHE}/debuginfo/{build_id}.debug") == redirect

      # The archive link is relative to the `debuginfo/` key, so it resolves
      # against the cache root - and must actually be fetchable.
      archive = parsed["archive"].replace("../", "", 1)
      client.succeed(f"{CURL} -sf {CACHE}/{archive} -o /dev/null")

      def status(url):
          return client.succeed(f"{CURL} -s -o /dev/null -w '%{{http_code}}' {url}").strip()

      assert status(f"{CACHE}/debuginfo/{'0' * 40}") == "404"
      assert status(f"{CACHE}/debuginfo/{'0' * 40}.debug") == "404"
      assert status(f"{CACHE}/debuginfo/not-a-build-id") == "404"
      # The exact request from #563: a debuginfo probe against a cache root that
      # has no such cache. Used to be a 400, which crashed the client.
      assert status(f"http://server/cache/debuginfo/{build_id}.debug") == "404"

      # ── Phase 10b: the worker reported a phase timeline for the build ────
      # Regression guard (#589): the timeline rides inside JobCompleted, so a
      # protocol or handler mistake shows up as a job with zero phases rather
      # than as an error anywhere. A build is dispatched once per anchor and its
      # record names whichever evaluation first named the derivation, so find it
      # through the evaluation's own build jobs, not through that attribution.
      banner("Phase 10b: the completed build job has worker phase spans")
      job_id = sql(
          f"SELECT dj.id FROM dispatched_job dj "
          f"JOIN build_job bj ON dj.job_id = 'build:' || bj.derivation_build::text "
          f"WHERE bj.evaluation = '{eval_id}' AND dj.kind = 1 AND dj.finished_at IS NOT NULL "
          f"ORDER BY dj.dispatched_at DESC LIMIT 1;"
      )
      assert job_id, "no finished build job was recorded for the evaluation"

      job = json.loads(api_get(token, f"board/jobs/{job_id}"))["message"]
      phases = {p["phase"] for p in job["phases"]}
      print(f"job {job_id} phases: {sorted(phases)}")
      assert "build" in phases, f"no build span in {sorted(phases)}"
      assert "nar_push" in phases, f"no nar push span in {sorted(phases)}"
      assert all(p["end_ms"] >= p["start_ms"] for p in job["phases"]), job["phases"]
      assert job["outcome"] == "completed", job["outcome"]
      assert job["finished_at"] is not None
      nested = [p for p in job["phases"] if p["parent_seq"] is not None]
      assert nested, "the timeline recorded no nesting at all"

      # The eval's phase columns are summed from its own timeline, so a zero
      # here means the eval job's spans never reached evaluation_metric.
      eval_ms = sql(
          f"SELECT fetch_ms + eval_flake_ms + eval_drv_ms FROM evaluation_metric "
          f"WHERE evaluation = '{eval_id}' LIMIT 1;"
      )
      assert eval_ms and int(eval_ms) > 0, f"eval phase columns not derived from the timeline: {eval_ms!r}"

      # ── Phase 10c: the reference counter moves with the cache (#592) ──────
      # Retire one of hello's runtime references: the zombie purge deletes the
      # row and ripples the loss up to every referrer, a re-upload seeds it
      # whole again and ripples that back. `missing_references` is moved, never
      # re-derived, so the recompute has to agree at every step.
      banner("Phase 10c: missing_references moves on retire and re-upload")

      retired_flag = int(sql(
          "SELECT count(*) FROM information_schema.columns "
          "WHERE table_name = 'cached_path' AND column_name = 'closure_complete';"
      ))
      assert retired_flag == 0, "cached_path still carries the retired closure_complete flag"

      # Phase 10d bills this cycle, so open the accounting before it runs. The
      # library counts from server start; the extension only exposes the view.
      sql("CREATE EXTENSION IF NOT EXISTS pg_stat_statements;")
      COUNTER_WRITES = "s.query ILIKE '%update cached_path%missing_references%'"
      counter_rows_before = int(sql(
          f"SELECT coalesce(sum(s.rows), 0) FROM pg_stat_statements s WHERE {COUNTER_WRITES};"
      ))

      def counter(path_hash):
          return int(sql(
              f"SELECT missing_references FROM cached_path WHERE hash = '{path_hash}';"
          ))

      def drift():
          return int(sql(
              "SELECT count(*) FROM cached_path cp WHERE cp.missing_references <> ("
              "  SELECT count(*) FROM cached_path_reference r "
              "  LEFT JOIN cached_path dep ON dep.hash = r.reference_hash "
              "  WHERE r.referrer = cp.hash AND r.reference_hash <> cp.hash "
              "    AND NOT (dep.file_hash IS NOT NULL AND dep.missing_references = 0));"
          ))

      # The anchor side of the same idea (#591): both readiness columns are moved
      # by the event that changes them, so a recompute has to agree with every row.
      # The `derivation_output` guard is not a tautology - `NOT EXISTS` is vacuous
      # for an anchor with no output rows, and without it every output-less
      # terminal-success anchor reads as fetchable, which is the unbacked-output
      # dead zone. The dependency count LEFT JOINs for the same reason the gate
      # does: a dependency with no anchor row at all counts as unready.
      def anchor_drift():
          return int(sql(
              "SELECT count(*) FROM derivation_build db WHERE db.unready_deps <> ("
              "  SELECT count(*) FROM derivation_dependency e "
              "  LEFT JOIN derivation_build dep ON dep.derivation = e.dependency "
              "  WHERE e.derivation = db.derivation "
              "    AND (dep.derivation IS NULL OR NOT dep.fetchable)) "
              "OR db.fetchable <> (db.substitutable OR (db.status IN (3, 7) AND EXISTS ("
              "  SELECT 1 FROM derivation_output o2 WHERE o2.derivation = db.derivation) "
              "AND NOT EXISTS ("
              "  SELECT 1 FROM derivation_output o LEFT JOIN cached_path cp ON cp.hash = o.hash "
              "  WHERE o.derivation = db.derivation "
              "    AND NOT (cp.file_hash IS NOT NULL AND cp.missing_references = 0))));"
          ))

      def poll(query, want, what, timeout=180):
          for _ in range(timeout):
              if sql(query) == want:
                  return
              server.sleep(1)
          raise Exception(f"{what} (still {sql(query)!r}, want {want!r})")

      assert drift() == 0, "counters disagree with their recompute before the retire"
      assert anchor_drift() == 0, "anchor counters disagree with their recompute before the retire"
      assert counter(store_hash) == 0, "hello's own output is not whole to start with"

      # glibc first, since hello links against it.
      refs = sql(
          f"SELECT cp.hash || '-' || cp.package FROM cached_path_reference r "
          f"JOIN cached_path cp ON cp.hash = r.reference_hash "
          f"WHERE r.referrer = '{store_hash}' AND r.reference_hash <> '{store_hash}' "
          f"  AND cp.file_hash IS NOT NULL AND cp.missing_references = 0 "
          f"ORDER BY (cp.package LIKE 'glibc-%') DESC, cp.package;"
      ).splitlines()

      # `NarStore` shards its objects by the store hash, under the module's baseDir.
      def nar_object(name):
          h = name.split("-")[0]
          return f"/var/lib/gradient/nars/{h[:2]}/{h[2:]}.nar.zst"

      # Both halves have to be real, and a whole `cached_path` row guarantees neither:
      # the path must be in the server's own store because the re-upload below runs the
      # CLI there, and the NAR object must still be on disk because a row outlives its
      # object until the zombie purge catches up. Picking on the row alone retired a
      # path whose object was already gone.
      dep_name = next(
          (n for n in (name.strip() for name in refs)
           if n
           and server.execute(f"test -e /nix/store/{n}")[0] == 0
           and server.execute(f"test -f {nar_object(n)}")[0] == 0),
          None,
      )
      assert dep_name, (
          f"none of hello's {len(refs)} whole references has both a path in the server "
          f"store and a NAR object on disk")
      dep_path = f"/nix/store/{dep_name}"
      dep_hash = dep_name.split("-")[0]
      dep_object = nar_object(dep_name)
      print(f"retiring {dep_path}")
      server.succeed(f"rm {dep_object}")

      # The zombie purge is the retiring caller here. The deep GC runs that same
      # pass now instead of waiting out cacheMaintenanceIntervalSecs.
      server.succeed(
          f"{CURL} -sf -X POST -H 'Authorization: Bearer {token}' "
          f"{API}/admin/maintenance/deep-gc"
      )
      poll(f"SELECT count(*) FROM cached_path WHERE hash = '{dep_hash}';", "0",
           "the zombie purge kept a row whose NAR is gone")

      missing = counter(store_hash)
      assert missing >= 1, f"hello's output should miss the retired path, has {missing}"
      assert drift() == 0, "counters disagree with their recompute after the retire"
      # The retire moves the anchor side in its own transaction: the producer of a
      # path it deleted stops being fetchable, and a terminal-success producer with
      # nothing left to serve is reset to a fresh build intent. Only the terminal
      # status and the flag are asserted, not `Created` exactly: the consistency
      # sweep may already have promoted the reset row, and re-queuing it is not the
      # bug this guards - a producer that stays trusted against a path that is gone
      # is.
      producer = sql(
          f"SELECT db.status::text || ' ' || db.fetchable::int::text FROM derivation_build db "
          f"JOIN derivation_output o ON o.derivation = db.derivation "
          f"WHERE o.hash = '{dep_hash}' LIMIT 1;"
      )
      assert producer, f"no anchor produces the retired output {dep_hash}; the check would pass on nothing"
      p_status, p_fetchable = producer.split()
      assert p_fetchable == "0" and p_status not in ("3", "7"), (
          f"the retired output's producer must lose fetchability and its terminal-success "
          f"status, has (status fetchable) = ({producer})"
      )
      hello_unready = int(sql(
          f"SELECT db.unready_deps FROM derivation_build db JOIN derivation d ON d.id = db.derivation "
          f"WHERE d.hash = '{drv_hash}';"
      ))
      assert hello_unready >= 1, f"hello must count its unfetchable dependencies as unready: {hello_unready}"

      # The other half of the reset's scope, and the one that costs a fleet when it
      # is wrong. hello only REFERENCES the retired path; its own output is still on
      # disk, so it loses fetchability and keeps its terminal status. Resetting the
      # referrer closure instead re-queued 107 derivations and dispatched 139 builds
      # in 30 s from this one deleted NAR, and rebuilt nothing that was missing.
      hello_anchor = sql(
          f"SELECT db.status::text || ' ' || db.fetchable::int::text FROM derivation_build db "
          f"JOIN derivation d ON d.id = db.derivation WHERE d.hash = '{drv_hash}';"
      )
      h_status, h_fetchable = hello_anchor.split()
      assert h_fetchable == "0" and h_status in ("3", "7"), (
          f"a referrer that only lost wholeness must keep its terminal-success status, or "
          f"the next evaluation rebuilds an output that never left the cache, has "
          f"(status fetchable) = ({hello_anchor})")
      assert anchor_drift() == 0, "anchor counters disagree with their recompute after the retire"

      print(server.succeed(f"{CLI} cache upload main {dep_path}"))
      poll(f"SELECT missing_references FROM cached_path WHERE hash = '{store_hash}';", "0",
           "the re-upload did not ripple hello's output back to whole")
      assert drift() == 0, "counters disagree with their recompute after the re-upload"
      assert anchor_drift() == 0, "anchor counters disagree with their recompute after the re-upload"

      # A re-upload restores wholeness, not trust. `fetchable` also needs a
      # terminal-success status, which only an evaluation's ingest or a finished
      # build writes, so a NAR arriving back in the cache must never re-trust the
      # producer the retire demoted. Asserted on that producer directly: hello's
      # own `unready_deps` looks like the same thing but is not, because it counts
      # one hop of `derivation_dependency` and the retired path is a runtime
      # reference, whose producer need not be an edge of hello at all. That form
      # read 0 as soon as wholeness rippled back and failed for the wrong reason.
      settled = sql(
          f"SELECT db.status::text || ' ' || db.fetchable::int::text FROM derivation_build db "
          f"JOIN derivation_output o ON o.derivation = db.derivation "
          f"WHERE o.hash = '{dep_hash}' LIMIT 1;"
      )
      s_status, s_fetchable = settled.split()
      assert s_fetchable == "0" or s_status in ("3", "7"), (
          f"the re-uploaded output's producer is fetchable again with no terminal-success "
          f"status, so the NAR alone re-trusted it; (status fetchable) = ({settled})")

      # The retire demoted the producer too, so the graph may rebuild and re-push
      # the path; phase 12b measures the drain of an *idle* worker, so wait that
      # self-heal out and re-check the counters it moved.
      poll("SELECT count(*) FROM dispatched_job WHERE finished_at IS NULL "
           "AND dispatched_at > (now() AT TIME ZONE 'UTC') - interval '10 minutes';",
           "0", "a re-dispatched build is still running", timeout=300)
      assert drift() == 0, "counters disagree with their recompute after the self-heal"
      assert anchor_drift() == 0, "anchor counters disagree with their recompute after the self-heal"

      # ── Phase 10d: the database-time bill (#592, #629) ────────────────────
      # The retired fixpoint was 67% of production database time, and nothing in
      # the type system notices a counter that is re-derived instead of moved: it
      # simply climbs this list. So print what the whole run cost, per statement,
      # and hold the counter's own writes to the referrers they touched.
      banner("Phase 10d: pg_stat_statements bills the run")

      def psql_table(query):
          server.succeed(f"cat > /tmp/q.sql <<'EOF'\n{query}\nEOF")
          return server.succeed("su postgres -c 'psql -d gradient -f /tmp/q.sql'")

      # The test's own psql connects as postgres; only the server's statements are
      # Gradient's bill, and excluding ours also keeps these polls out of the shares.
      server_statements = (
          "FROM pg_stat_statements s JOIN pg_roles r ON r.oid = s.userid "
          "WHERE r.rolname <> 'postgres' "
      )
      print(psql_table(
          "SELECT regexp_replace(substring(s.query, 1, 250), '[[:space:]]+', ' ', 'g') AS query, "
          "s.calls, round(s.total_exec_time::numeric, 2) AS total_time, "
          "round(s.mean_exec_time::numeric, 2) AS mean_time, "
          "round((100 * s.total_exec_time / sum(s.total_exec_time) OVER ())::numeric, 2) AS percentage "
          + server_statements +
          "ORDER BY s.total_exec_time DESC LIMIT 10;"
      ))

      counter_rows = int(sql(
          f"SELECT coalesce(sum(s.rows), 0) FROM pg_stat_statements s WHERE {COUNTER_WRITES};"
      )) - counter_rows_before
      cached_paths = int(sql("SELECT count(*) FROM cached_path;"))
      assert 0 < counter_rows <= cached_paths, (
          f"one retire and one re-upload wrote {counter_rows} counter rows over a "
          f"{cached_paths}-row cache; a moved counter touches referrers, a derived one the table"
      )

      # Loose on purpose: these are pathology detectors on a slow shared VM, not
      # benchmarks. A per-tick fixpoint over the cache breaks both by an order of
      # magnitude, and the printout above is what a human reads.
      total_ms = float(sql(
          f"SELECT round(coalesce(sum(s.total_exec_time), 0)::numeric, 2) {server_statements};"
      ))
      counter_share = float(sql(
          f"SELECT round(coalesce(100 * sum(s.total_exec_time) FILTER (WHERE {COUNTER_WRITES}) "
          f"/ nullif(sum(s.total_exec_time), 0), 0)::numeric, 2) {server_statements};"
      ))
      print(f"server database time: {total_ms} ms, reference counter writes: {counter_share}%")
      assert total_ms < 300000, f"the run burned {total_ms} ms of database time"
      assert counter_share < 50, f"maintaining the counter is {counter_share}% of database time"

      # ── Phase 10e: a re-evaluation settles the producer a retire reset ─────
      # The retire reset the producer of the path it DELETED back to Created, and a
      # re-upload does not settle it: `fetchable` reads the anchor's own status, so
      # only an evaluation (the ingest marks a derivation whole in our cache
      # Substituted) or a rebuild puts it back. Referrers were never reset, so they
      # need nothing here. This phase drives the
      # evaluation and asserts the graph converges: the producer is fetchable again,
      # hello's counter is back to zero, and neither counter disagrees with its
      # recompute. The polling trigger is configured at 10 s on `task`, so an empty
      # commit is picked up like phase 4's initial commit.
      banner("Phase 10e: the next evaluation finds every output whole and settles the graph")

      # The consistency sweep is the other claimant for those same Created anchors:
      # it promotes them on its own 300 s cadence and the fleet re-pushes them, so
      # wait that cascade out before sampling, or its dispatches are billed to the
      # re-evaluation below.
      poll("SELECT count(*) FROM dispatched_job WHERE finished_at IS NULL "
           "AND dispatched_at > (now() AT TIME ZONE 'UTC') - interval '10 minutes';",
           "0", "a re-dispatched build is still running", timeout=300)
      builds_before = int(sql("SELECT count(*) FROM dispatched_job WHERE kind = 1;"))

      server.succeed(f"{GIT} -C /var/lib/git/test commit --allow-empty -m 'retrigger'")
      server.succeed("chown git:git -R /var/lib/git/test")
      eval3_id = ""
      for attempt in range(1, 61):
          server.sleep(10)
          candidate = server.succeed(
              f'{CURL} -sf -H "Authorization: Bearer {token}" '
              f'{API}/tasks/project/task | {JQ} -rj ".message.last_evaluation // empty"'
          ).strip()
          if candidate and candidate not in (eval_id, eval2_id):
              status3 = server.succeed(
                  f'{CURL} -sf -H "Authorization: Bearer {token}" '
                  f'{API}/evals/{candidate} | {JQ} -rj ".message.status"'
              ).strip()
              if status3 == "Completed":
                  eval3_id = candidate
                  break
              if status3 == "Failed":
                  j = server.succeed("journalctl -u gradient-server --no-pager --since='-600s' -n 300")
                  raise Exception(f"the re-evaluation failed:\n{j[-3000:]}")
      assert eval3_id, "the re-evaluation did not complete after 600 s"

      producer = sql(
          f"SELECT db.status::text || ' ' || db.fetchable::int::text FROM derivation_build db "
          f"JOIN derivation_output o ON o.derivation = db.derivation "
          f"WHERE o.hash = '{dep_hash}' LIMIT 1;"
      )
      assert producer in ("3 1", "7 1"), (
          f"the retired output's producer must be terminal-success and fetchable again "
          f"after the re-evaluation, has (status fetchable) = ({producer})"
      )
      assert int(sql(
          f"SELECT db.unready_deps FROM derivation_build db JOIN derivation d ON d.id = db.derivation "
          f"WHERE d.hash = '{drv_hash}';"
      )) == 0, "hello's counter must return to zero"
      unsettled = int(sql(
          f"SELECT count(*) FROM build_job bj "
          f"JOIN derivation_build db ON db.derivation = bj.derivation "
          f"WHERE bj.evaluation = '{eval3_id}' AND (db.status NOT IN (3, 7) OR NOT db.fetchable);"
      ))
      assert unsettled == 0, f"{unsettled} anchors of the completed re-evaluation are not settled and fetchable"
      assert drift() == 0, "counters disagree with their recompute after the re-evaluation"
      assert anchor_drift() == 0, "anchor counters disagree with their recompute after the re-evaluation"

      # Printed, not asserted: a whole cache needs no build, but the sweep above can
      # promote the same anchors inside this window and its dispatches are
      # indistinguishable from the evaluation's, so a zero here would be a coin flip.
      builds_after = int(sql("SELECT count(*) FROM dispatched_job WHERE kind = 1;"))
      print(f"builds dispatched around the re-evaluation: {builds_after - builds_before}")

      # ── Phase 10f: the ordered lock is what makes a recount correct ───────
      # Four defect classes on the counter stack were a counter written outside an
      # ordered lock, and none of them had a test that would fail. This pins ONE
      # interleaving, the one the readiness repair's lock exists for: a retire holds
      # its `cached_path` lock and the anchor lock its readiness half takes, while a
      # `fetchable` recount waits behind that anchor lock. An UNLOCKED recount reads
      # its new value from a snapshot taken before the retire commits and its
      # compare-and-swap from the fresh row, so it stores `true` for an anchor whose
      # only output the retire just deleted - and `fetchable = true` is exactly what
      # stops it counting toward its dependents' `unready_deps`. Under the lock the
      # recount's statement opens after that commit and stores the true value.
      #
      # Both `psql_*` helpers run a fresh process per call, so no transaction can
      # survive between driver steps; dblink cannot block on a row lock and then
      # proceed. So one script on the VM owns both sessions: two psql processes fed
      # from FIFOs, and a third connection polling `pg_stat_activity` to know that
      # session B is genuinely blocked before session A commits. Every wait is
      # bounded, every exit path closes both sessions, and every row it touches is
      # one it created.
      banner("Phase 10f: a retire holds its locks while a readiness recount waits")
      LR_DRV = "aaaaaaaa-0000-4000-8000-00000000fe01"
      LR_DRV2 = "aaaaaaaa-0000-4000-8000-00000000fe02"
      lr_drv_hash = "lockraced".ljust(32, "0")
      lr_out_hash = "lockraceo".ljust(32, "0")
      lr_drv2_hash = "lockracee".ljust(32, "0")
      lr_out2_hash = "lockracep".ljust(32, "0")

      def lockrace_fixture(drv, drv_hash, out_hash, name):
          sql(
              f"INSERT INTO derivation (id, created_at, architecture, hash, name, "
              f"prefer_local_build, allow_substitutes, is_fixed_output, walked) VALUES "
              f"('{drv}', now() AT TIME ZONE 'UTC', 'x86_64-linux', '{drv_hash}', "
              f"'{name}', false, true, false, false);\n"
              f"INSERT INTO derivation_build (id, derivation, status, substitutable, substituted, "
              f"fetchable, unready_deps, attempt, created_at, updated_at) VALUES "
              f"(uuidv7(), '{drv}', 3, false, false, false, 0, 0, "
              f"now() AT TIME ZONE 'UTC', now() AT TIME ZONE 'UTC');\n"
              f"INSERT INTO derivation_output (id, derivation, name, hash, package, is_cached, "
              f"created_at) VALUES (uuidv7(), '{drv}', 'out', '{out_hash}', '{name}-out', "
              f"true, now() AT TIME ZONE 'UTC');\n"
              f"INSERT INTO cached_path (id, hash, package, file_hash, file_size, nar_size, nar_hash, "
              f"missing_references, created_at) VALUES (uuidv7(), '{out_hash}', '{name}-out', "
              f"'sha256:lockrace', 1, 1, 'sha256:lockrace', 0, now() AT TIME ZONE 'UTC');"
          )
          assert sql(
              f"SELECT db.fetchable::int::text || ' ' || (SELECT count(*)::text FROM cached_path "
              f"WHERE hash = '{out_hash}' AND file_hash IS NOT NULL AND missing_references = 0) "
              f"FROM derivation_build db WHERE db.derivation = '{drv}';"
          ) == "0 1", f"the {name} fixture did not land as a drifted anchor over a whole output"

      def lockrace_cleanup(drv, out_hash):
          sql(
              f"DELETE FROM cached_path WHERE hash = '{out_hash}';\n"
              f"DELETE FROM derivation_output WHERE derivation = '{drv}';\n"
              f"DELETE FROM derivation_build WHERE derivation = '{drv}';\n"
              f"DELETE FROM derivation WHERE id = '{drv}';"
          )

      # A terminal-success anchor with one whole output and `fetchable` stored false:
      # drifted by construction, which is what the repair exists to correct and what
      # makes a stale recount write the wrong value instead of nothing. Two of them,
      # one per arm, so the arms cannot interfere.
      lockrace_fixture(LR_DRV, lr_drv_hash, lr_out_hash, "lockrace-probe")
      lockrace_fixture(LR_DRV2, lr_drv2_hash, lr_out2_hash, "lockrace-unlocked")

      server.succeed(f"cat > /tmp/lockrace.sh <<'LOCKRACE'\n{LOCK_RACE_SH}\nLOCKRACE")
      print(server.succeed(
          f"LR_DRV={LR_DRV} LR_OUT={lr_out_hash} "
          f"LR_DRV2={LR_DRV2} LR_OUT2={lr_out2_hash} sh /tmp/lockrace.sh"
      ))

      locked = sql(f"SELECT db.fetchable::int FROM derivation_build db WHERE db.derivation = '{LR_DRV}';")
      unlocked = sql(f"SELECT db.fetchable::int FROM derivation_build db WHERE db.derivation = '{LR_DRV2}';")
      rows_left = int(sql(
          f"SELECT count(*) FROM cached_path WHERE hash IN ('{lr_out_hash}', '{lr_out2_hash}');"
      ))
      # The unlocked arm leaves a deliberately wrong `fetchable`, so both fixtures come
      # out before the drift checks, which would otherwise count the defect we asked for.
      lockrace_cleanup(LR_DRV, lr_out_hash)
      lockrace_cleanup(LR_DRV2, lr_out2_hash)
      race_drift = drift()
      race_anchor_drift = anchor_drift()

      assert rows_left == 0, "a retire session did not commit its delete"
      assert locked == "0", (
          f"the locked recount stored fetchable = {locked!r} for an anchor whose only output "
          f"the retire deleted: its snapshot was not ordered after that commit"
      )
      assert unlocked == "1", (
          f"the UNLOCKED recount stored fetchable = {unlocked!r}, so it did not write the stale "
          f"true this lock exists to prevent. The two arms now agree, which means taking the "
          f"anchor lock in its own statement is no longer what makes the recount correct: "
          f"re-derive the discipline in gradient_db::readiness before trusting it"
      )
      assert race_drift == 0 and race_anchor_drift == 0, (
          f"counters disagree with their recompute after the lock race "
          f"(nar {race_drift}, anchor {race_anchor_drift})"
      )

      # ── Phase 11: the supervision tree is healthy and shutdown drains ─────
      banner("Phase 11: every supervised loop is running; SIGTERM drains")
      health = json.loads(api_get(token, "board/health"))["message"]
      names = sorted(l["name"] for l in health["supervised"])
      print(names)
      for want in ["graph", "build-dispatch", "eval-dispatch", "trigger-dispatch",
                   "cache-maintenance", "sign-sweep", "debug-index",
                   "eval-cache-sweep", "retention", "rollup", "outbound-connect"]:
          assert want in names, f"{want} missing from supervised loops: {names}"
      bad = [l for l in health["supervised"] if l["restarts"] or l["pass_timeouts"]]
      assert not bad, f"restarted or stalled loops: {bad}"
      assert health["proto_sessions"] >= 1, health

      t0 = time.time()
      server.succeed("systemctl stop gradient-server.service")
      stop_secs = time.time() - t0
      print(f"gradient-server stopped in {stop_secs:.1f}s")
      assert stop_secs < 40, f"shutdown took {stop_secs:.1f}s; the drain budget is 30s"
      server.succeed("journalctl -u gradient-server --no-pager | grep -q 'background tasks drained cleanly'")
      builder.succeed("journalctl -u gradient-worker --no-pager | grep -q 'server is draining'")

      # ── Phase 12: a drained server does not decommission the worker (#626) ─
      # The worker used to exit(0) on `Draining`, and because the unit restarts
      # `on-failure` that left the fleet dead after every deploy until someone
      # restarted each worker by hand.
      banner("Phase 12: the worker survives the drain and reconnects")
      builder.succeed("systemctl is-active gradient-worker.service")
      builder.succeed("journalctl -u gradient-worker --no-pager | grep -q 'server drained the session'")

      server.succeed("systemctl start gradient-server.service")
      server.wait_for_open_port(3000)
      builder.wait_until_succeeds(
          "journalctl -u gradient-worker --no-pager | grep -q 'reconnected successfully'", timeout=180
      )

      # A local signal is the only thing that stops a worker: it drains first,
      # then exits cleanly (the unit is idle here, so this is immediate).
      banner("Phase 12b: SIGTERM drains the worker, then stops it")
      t0 = time.time()
      builder.succeed("systemctl stop gradient-worker.service")
      stop_secs = time.time() - t0
      print(f"gradient-worker stopped in {stop_secs:.1f}s")
      assert stop_secs < 40, f"an idle worker took {stop_secs:.1f}s to drain"
      builder.succeed(
          "journalctl -u gradient-worker --no-pager | grep -q 'draining: no new jobs'"
      )
      assert builder.succeed(
          "systemctl show -p Result --value gradient-worker.service"
      ).strip() == "success"

      banner("Cache test PASSED")
      '';
  });
}
