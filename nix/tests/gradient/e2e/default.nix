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

      # One job per core. At 8 the builder kernel-panicked on OOM mid-run
      # (`compulsory panic_on_oom`) with 2048 MB and four cores, and the
      # oversubscription bought nothing: the cores were already the limit.
      max-jobs = lib.mkForce 4;
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
    name = "gradient-e2e";
    # Phases 10e to 10h add five more evaluations of the repository, a two-session
    # lock handshake and four retire-and-recover cycles to what was already a full
    # build-and-cache run, and 10g only began relaying for real once it stopped
    # failing in its first seconds.
    globalTimeout = 5400;

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

        # Phase 10g serves busybox's closure from this host as a file binary
        # cache. An anchor is substitutable only when EVERY output is on an
        # upstream, so the `debug` output has to be in the store to be copied
        # there; `systemPackages` brings only `out`.
        virtualisation.additionalPaths = [ pkgs.busybox.debug ];

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

            # Signs the file binary cache phase 10g substitutes from. Fixed
            # rather than generated in the test: the cache is state-managed, so
            # its upstream is declared below and the public half has to be known
            # at evaluation time.
            "gradient/secrets/upstream_key" = {
              mode = "0600";
              text = "file-upstream-1:eZPukjHYgRpJ+hLlnUgG+qpi/k4QMTr3bd4ftngZJwkIXutyHrlDclZGwczy+IKCAt82HkKrDtM16u9HR9uzkQ==";
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
          # `hook.local` is phase 14's webhook target. A name, not `127.0.0.1`:
          # `validate_webhook_url` rejects every loopback and private literal, and
          # every address in a NixOS VM network is one of those.
          "127.0.0.1" = [ "gradient.local" "hook.local" ];
        };

        services = {
          gradient = {
            enable = true;
            reverseProxy.nginx.enable = true;
            configurePostgres = true;
            # Pinned so the module's production-sized default does not size a
            # 2 GB guest that also runs the server, nginx and a git daemon.
            postgresSharedBuffers = "128MB";
            domain = "gradient.local";
            proto.public = true;
            jwtSecretFile = toString (pkgs.writeText "jwtSecret" "b68a8eaa8ebcff23ebaba1bd74ecb8a2eb7ba959570ff8842f148207524c7b8d731d7a1998584105e951599221f9dcd20e41223be17275ca70ab6f7e6ecafa8d4f8905623866edb2b344bd15de52ccece395b3546e2f00644eb2679cf7bdaa156fd75cc5f47c34448cba19d903e68015b1ad3c8e9d04862de0a2c525b6676779012919fa9551c4746f9323ab207aedae86c28ada67c901cae821eef97b69ca4ebe1260de31add34d8265f17d9c547e3bbabe284d9cadcc22063ee625b104592403368090642a41967f8ada5791cb09703d0762a3175d0fe06ec37822e9e41d0a623a6349901749673735fdb94f2c268ac08a24216efb058feced6e785f34185a");
            cryptSecretFile = toString (pkgs.writeText "cryptSecret" "aW52YWxpZC1pbnZhbGlkLWludmFsaWQK");
            settings = {
              logLevel.default = "debug";
              # Phase 10i waits out a cache-maintenance pass, and the hourly
              # default would outlast the test. Every step of that pass is a
              # no-op at this scale except the one the phase drives.
              cacheMaintenanceIntervalSecs = 20;
              cacheTtlHours = 1;
            };
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
                  # Phase 14's probe. It lives in state because provisioning runs
                  # on every boot and deletes actions state does not declare, so
                  # an API-created one would not survive that phase's restart; it
                  # subscribes to an event this VM never raises, so the only thing
                  # that ever reaches the hook is the row the phase writes.
                  actions = [
                    {
                      name = "restart-probe";
                      type = "send_web_request";
                      events = [ "evaluation.approval_granted" ];
                      config = { url = "http://hook.local:8099/hook"; };
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
                  upstreams = [{
                    type = "external";
                    display_name = "file-upstream";
                    # `server`, not `gradient.local`: the RELAY runs on a builder,
                    # and only the server node maps the gradient.local name. An
                    # upstream a worker cannot resolve is not an upstream.
                    url = "http://server/upstream";
                    public_key = "file-upstream-1:CF7rch65Q3JWRsHM8viCggLfNh5Cqw7TNervR0fbs5E=";
                  }];
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
            # Phase 10g's upstream binary cache, served off disk from this same
            # host so the VM needs no network.
            locations."/upstream/" = {
              alias = "/srv/upstream/";
              extraConfig = "autoindex off;";
            };
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
          "d /srv/upstream 0755 nginx nginx"
          "L+ /var/lib/git/flake.nix 0755 git git - ${./flake_repository.nix}"
          "L+ /var/lib/git/flake.lock 0755 git git - ${./flake_repository.lock}"
          "L+ /var/lib/git/flake-busywrap.nix 0755 git git - ${./flake_repository_busywrap.nix}"
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
              f'{CURL} -sf -H "Authorization: Bearer {token}" "{API}/{path}"'
          )

      def blocking_anchors(evaluation):
          """The anchors keeping `evaluation` in its build phase, bucketed by the
          terms of `gates_predicate`. A `Created` row's `fetchable` is false by
          definition, so a status histogram carries no information about WHY one
          is not queued; these booleans do. `probed` is not a gate on the row it
          is printed for: it is why the rows BELOW it are missing from this list,
          which is the one shape the others cannot show."""
          return sql(
              f"SELECT status::text || ' walked=' || walked::int::text"
              f"  || ' drv=' || drv::int::text"
              f"  || ' substitutable=' || substitutable::int::text"
              f"  || ' probed=' || probed::int::text"
              f"  || ' deps_ready=' || deps_ready::int::text"
              f"  || ' present=' || present::int::text"
              f"  || ' whole=' || whole::int::text"
              f"  || ' count=' || count(*)::text"
              f" FROM (SELECT db.status, w.walked, db.substitutable, db.probed,"
              f"              EXISTS (SELECT 1 FROM cached_path cp"
              f"                      WHERE cp.hash = w.hash AND cp.file_hash IS NOT NULL) AS drv,"
              f"              db.unready_deps = 0 AS deps_ready,"
              f"              db.missing_runtime_deps = 0 AS whole,"
              f"              NOT EXISTS (SELECT 1 FROM derivation_output o"
              f"                          LEFT JOIN cached_path cp"
              f"                            ON cp.hash = o.hash AND cp.file_hash IS NOT NULL"
              f"                          WHERE o.derivation = db.derivation"
              f"                            AND cp.hash IS NULL) AS present"
              f"       FROM derivation_build db"
              f"       JOIN derivation w ON w.id = db.derivation"
              f"       JOIN build_job bj ON bj.derivation_build = db.id"
              f"       WHERE bj.evaluation = '{evaluation}'"
              f"         AND db.status IN (0, 1, 2, 8)"
              f"         AND (db.demanded OR db.status IN (1, 2))) g"
              f" GROUP BY status, walked, drv, substitutable, probed, deps_ready, present, whole"
              f" ORDER BY 1;"
          )

      def unready_reasons(evaluation):
          """Why each blocking anchor of `evaluation` still counts an unready
          dependency, named on both sides. `unready_deps` is a number; a hang needs
          the edge it stands for and the dependency's own status, wholeness and
          presence, which is what stops it being fetchable."""
          present = (
              "NOT EXISTS (SELECT 1 FROM derivation_output o"
              "            LEFT JOIN cached_path cp"
              "              ON cp.hash = o.hash AND cp.file_hash IS NOT NULL"
              "            WHERE o.derivation = dep.derivation AND cp.hash IS NULL)"
          )
          fetchable = f"dep.status IN (3, 7) AND dep.missing_runtime_deps = 0 AND {present}"
          return sql(
              f"SELECT d.name || ' [' || db.status::text || ']  needs  ' || dn.name"
              f"    || ' [' || coalesce(dep.status::text, 'no anchor')"
              f"    || ' kind=' || e.kind::text"
              f"    || ' whole=' || coalesce((dep.missing_runtime_deps = 0)::int::text, '-')"
              f"    || ' present=' || coalesce(({present})::int::text, '-') || ']'"
              f" FROM derivation_build db"
              f" JOIN derivation d ON d.id = db.derivation"
              f" JOIN build_job bj ON bj.derivation_build = db.id"
              f" JOIN derivation_dependency e ON e.derivation = db.derivation"
              f" JOIN derivation dn ON dn.id = e.dependency"
              f" LEFT JOIN derivation_build dep ON dep.derivation = e.dependency"
              f" WHERE bj.evaluation = '{evaluation}'"
              f"   AND db.status IN (0, 1, 2, 8) AND (db.demanded OR db.status IN (1, 2))"
              f"   AND (dep.derivation IS NULL OR NOT ({fetchable}))"
              f" ORDER BY 1 LIMIT 40;"
          )

      def assert_no_server_error(j):
          """The lines a healthy run never writes, named rather than counted. A bare
          `needle in j` says a pool timed out somewhere in the last 900 s, which
          names neither the pool, the module nor how often; the slowest statements
          below name what was holding the connections."""
          hits = {}
          for needle in ("pool timed out", "graph call timed out", "graph actor unreachable",
                         "ingest transaction failed", "dropped as stale"):
              lines = [line[-300:] for line in j.splitlines() if needle in line]
              if lines:
                  hits[needle] = lines
          if not hits:
              return

          report = "\n\n".join(
              f"{needle!r}, {len(lines)} lines:\n" + "\n".join(lines[:6])
              for needle, lines in hits.items()
          )
          refused = server.succeed(
              "journalctl --no-pager | grep -c 'too many clients already' || true"
          ).strip()
          sql("CREATE EXTENSION IF NOT EXISTS pg_stat_statements;")
          slowest = sql(
              "SELECT round(s.max_exec_time::numeric, 0) || ' ms max, ' || s.calls || ' calls: '"
              "    || regexp_replace(substring(s.query, 1, 160), '[[:space:]]+', ' ', 'g')"
              " FROM pg_stat_statements s JOIN pg_roles r ON r.oid = s.userid"
              " WHERE r.rolname <> 'postgres'"
              " ORDER BY s.max_exec_time DESC LIMIT 10;"
          )
          raise Exception(
              f"the server log carries what a healthy run does not:\n{report}\n\n"
              f"postgres refused a connection {refused} times, so a pool timeout above "
              f"is a busy pool and not a cluster at its ceiling\n\n"
              f"the server's slowest single executions:\n{slowest}"
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

      # The arm is only a race once the retire owns a row. A fixture deleted out from
      # under it leaves both sessions unblocked and the next wait times out blaming
      # the recount, so name the real cause here.
      retired() {
        if [ "$(grep -c '^DELETE 1$' $D/a.out)" != "$1" ]; then
          echo "LOCKRACE: the retire deleted no row, its fixture was gone before the arm ran"
          cat $D/a.out
          exit 1
        fi
      }

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
        printf '%s\\n' "UPDATE derivation_build db SET fetchable = x.f FROM (SELECT p.derivation, p.fetchable AS old, (p.substitutable OR (p.status IN (3, 7) AND p.missing_runtime_deps = 0 AND EXISTS (SELECT 1 FROM derivation_output o2 WHERE o2.derivation = p.derivation) AND NOT EXISTS (SELECT 1 FROM derivation_output o LEFT JOIN cached_path cp ON cp.hash = o.hash WHERE o.derivation = p.derivation AND cp.file_hash IS NULL))) AS f FROM derivation_build p WHERE p.derivation = ANY(ARRAY['$1']::uuid[])) x WHERE db.derivation = x.derivation AND db.fetchable = x.old AND x.old <> x.f;"
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
      retired 1

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
      retired 2

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

      # Phase 10i's two-session interleaving, on the phase 10f scaffolding. It reads
      # CR_EVAL, CR_ANCHOR and CR_HOLD from the environment, parks the evaluation in
      # its build phase behind CR_HOLD, races a naming of CR_ANCHOR against its
      # transition, then releases CR_HOLD. Nothing locks, so the naming counts the
      # anchor it cannot see move: the server's exact reads have to repair that.
      COUNTER_RACE_SH = """
      set -u
      D=/tmp/counterrace
      APID=""
      BPID=""

      cleanup() {
        exec 3>&-
        exec 4>&-
        if [ -n "$APID" ]; then kill $APID 2>/dev/null || true; fi
        if [ -n "$BPID" ]; then kill $BPID 2>/dev/null || true; fi
        mkdir -p $D
        printf '%s\\n' "SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE application_name LIKE 'counterrace_%';" > $D/kill.sql
        su postgres -c "psql -d gradient -At -q -f $D/kill.sql" >/dev/null 2>&1 || true
        rm -rf $D
      }
      trap cleanup EXIT INT TERM

      q() {
        printf '%s\\n' "$1" > $D/q.sql
        su postgres -c "psql -d gradient -At -q -v ON_ERROR_STOP=1 -f $D/q.sql"
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
        echo "COUNTERRACE TIMEOUT: $2"
        cat $D/a.out || true
        cat $D/b.out || true
        q "SELECT pid, application_name, state, wait_event_type, wait_event, left(query, 90) FROM pg_stat_activity WHERE datname = 'gradient' ORDER BY pid"
        exit 1
      }

      send_a() { printf '%s\\n' "$1" >&3; }
      send_b() { printf '%s\\n' "$1" >&4; }

      name() {
        echo "INSERT INTO build_job (id, evaluation, derivation, derivation_build, score, score_breakdown, created_at) SELECT uuidv7(), '$CR_EVAL', db.derivation, db.id, 0, '{}'::jsonb, now() AT TIME ZONE 'UTC' FROM derivation_build db WHERE db.id = '$1';"
      }

      rm -rf $D
      mkdir -p $D

      q "BEGIN; SELECT pg_advisory_xact_lock(640); UPDATE evaluation SET status = 3, waiting_reason = NULL WHERE id = '$CR_EVAL'; $(name "$CR_HOLD") WITH gone AS (DELETE FROM evaluation_anchor_delta WHERE evaluation = '$CR_EVAL' RETURNING 1), c AS (SELECT count(bj.id)::int AS named, coalesce(sum(x.active), 0)::int AS active, coalesce(sum(x.failed), 0)::int AS failed, coalesce(sum(x.queued), 0)::int AS queued, coalesce(sum(x.building), 0)::int AS building FROM build_job bj JOIN derivation_build db ON db.id = bj.derivation_build CROSS JOIN LATERAL evaluation_anchor_counts(db.status, db.demanded) x WHERE bj.evaluation = '$CR_EVAL') UPDATE evaluation e SET named_anchors = c.named, active_anchors = c.active, failed_anchors = c.failed, queued_anchors = c.queued, building_anchors = c.building FROM c WHERE e.id = '$CR_EVAL'; COMMIT;" >/dev/null

      mkfifo $D/a.in
      mkfifo $D/b.in
      su postgres -c "psql -d gradient -At" < $D/a.in > $D/a.out 2>&1 &
      APID=$!
      su postgres -c "psql -d gradient -At" < $D/b.in > $D/b.out 2>&1 &
      BPID=$!
      exec 3> $D/a.in
      exec 4> $D/b.in
      send_a "SET application_name = 'counterrace_a';"
      send_b "SET application_name = 'counterrace_b';"

      send_a "BEGIN;"
      send_a "UPDATE derivation_build SET status = 3 WHERE id = '$CR_ANCHOR';"
      wait_state "application_name = 'counterrace_a' AND state = 'idle in transaction'" "the move never held its row"
      send_b "BEGIN;"
      send_b "$(name "$CR_ANCHOR")"
      wait_state "application_name = 'counterrace_b' AND state = 'idle in transaction'" "the naming waited on the move: a trigger took a lock"
      send_b "COMMIT;"
      wait_state "application_name = 'counterrace_b' AND state = 'idle'" "the naming never committed"
      send_a "COMMIT;"
      wait_state "application_name = 'counterrace_a' AND state = 'idle'" "the move never committed"

      if grep -q ERROR $D/a.out $D/b.out; then
        echo "COUNTERRACE: a session reported an error"
        cat $D/a.out
        cat $D/b.out
        exit 1
      fi

      q "UPDATE derivation_build SET status = 3 WHERE id = '$CR_HOLD';" >/dev/null
      echo "counterrace: raced and released"
      """

      # Phase 10j's two-session claim race. It reads CL_ANCHOR, CL_EVAL and
      # CL_PROJECT from the environment and runs the claim of
      # `gradient_db::claim_dispatch` for one job key in two sessions at once: the
      # second must block on the unique open-row index and insert nothing.
      CLAIM_RACE_SH = """
      set -u
      D=/tmp/claimrace
      APID=""
      BPID=""

      cleanup() {
        exec 3>&-
        exec 4>&-
        if [ -n "$APID" ]; then kill $APID 2>/dev/null || true; fi
        if [ -n "$BPID" ]; then kill $BPID 2>/dev/null || true; fi
        mkdir -p $D
        printf '%s\\n' "SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE application_name LIKE 'claimrace_%';" > $D/kill.sql
        su postgres -c "psql -d gradient -At -q -f $D/kill.sql" >/dev/null 2>&1 || true
        rm -rf $D
      }
      trap cleanup EXIT INT TERM

      q() {
        printf '%s\\n' "$1" > $D/q.sql
        su postgres -c "psql -d gradient -At -q -v ON_ERROR_STOP=1 -f $D/q.sql"
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
        echo "CLAIMRACE TIMEOUT: $2"
        cat $D/a.out || true
        cat $D/b.out || true
        exit 1
      }

      claim() {
        echo "INSERT INTO dispatched_job (id, kind, evaluation_id, project, worker_id, job_id, score, queued_at, dispatched_at, score_breakdown, worker_context, job_context, created_at) SELECT uuidv7(), 1, '$CL_EVAL', '$CL_PROJECT', '$1', 'build:$CL_ANCHOR', 0, now() AT TIME ZONE 'UTC', now() AT TIME ZONE 'UTC', '{}'::jsonb, '{}'::jsonb, '{}'::jsonb, now() AT TIME ZONE 'UTC' WHERE EXISTS (SELECT 1 FROM derivation_build WHERE id = '$CL_ANCHOR' AND status = 1 AND substitutable = false) ON CONFLICT (job_id) WHERE finished_at IS NULL DO NOTHING;"
      }

      rm -rf $D
      mkdir -p $D
      mkfifo $D/a.in
      mkfifo $D/b.in
      su postgres -c "psql -d gradient -At" < $D/a.in > $D/a.out 2>&1 &
      APID=$!
      su postgres -c "psql -d gradient -At" < $D/b.in > $D/b.out 2>&1 &
      BPID=$!
      exec 3>$D/a.in
      exec 4>$D/b.in

      printf '%s\\n' "SET application_name = 'claimrace_a';" >&3
      printf '%s\\n' "SET application_name = 'claimrace_b';" >&4

      printf '%s\\n' "BEGIN;" >&3
      printf '%s\\n' "$(claim instance-a)" >&3
      wait_state "application_name = 'claimrace_a' AND state = 'idle in transaction'" "the first claim never inserted"
      printf '%s\\n' "BEGIN;" >&4
      printf '%s\\n' "$(claim instance-b)" >&4
      wait_state "application_name = 'claimrace_b' AND wait_event_type = 'Lock'" "the second claim did not wait on the open-row index"
      printf '%s\\n' "COMMIT;" >&3
      wait_state "application_name = 'claimrace_a' AND state = 'idle'" "the first claim never committed"
      wait_state "application_name = 'claimrace_b' AND state = 'idle in transaction'" "the second claim never finished"
      printf '%s\\n' "COMMIT;" >&4
      wait_state "application_name = 'claimrace_b' AND state = 'idle'" "the second claim never committed"

      if grep -q ERROR $D/a.out $D/b.out; then
        echo "CLAIMRACE: a session reported an error"
        cat $D/a.out
        cat $D/b.out
        exit 1
      fi

      echo "claimrace_a $(grep INSERT $D/a.out)"
      echo "claimrace_b $(grep INSERT $D/b.out)"
      """

      # Phase 10k's two-session interleaving of a seed with a flip (#643). It runs the
      # registry's own statements, printed by `gradient-sql-gate --print`, on rows it
      # owns in a scratch schema whose tables copy the real ones without foreign keys,
      # and after each arm the table-wide recount must find nothing to correct. The
      # unguarded arm is the contrast: the same interleaving under the flip's lock
      # alone must drift, or the shared keys are no longer what keeps the count right.
      LOCK_GUARD_SH = r"""
      set -u
      D=/tmp/lockguard
      APID=""
      BPID=""
      P=00000000-0000-4000-8000-00000000000a
      DEP=00000000-0000-4000-8000-00000000000b
      Q=00000000-0000-4000-8000-00000000000c

      pg() { eval "$LG_PSQL"; }

      cleanup() {
        exec 3>&- 2>/dev/null
        exec 4>&- 2>/dev/null
        if [ -n "$APID" ]; then kill $APID 2>/dev/null || true; fi
        if [ -n "$BPID" ]; then kill $BPID 2>/dev/null || true; fi
        mkdir -p $D
        printf '%s\n' "SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE application_name LIKE 'lockguard_%';" | pg >/dev/null 2>&1 || true
        rm -rf $D
      }
      trap cleanup EXIT INT TERM

      q() { printf '%s\n' "SET search_path = lockguard, public;" "$1" | pg | grep -v '^SET$'; }

      fail() {
        echo "LOCKGUARD: $1"
        echo "--- session a ---"; cat $D/a.out || true
        echo "--- session b ---"; cat $D/b.out || true
        q "SELECT pid, application_name, state, wait_event_type, wait_event, left(query, 90) FROM pg_stat_activity WHERE application_name LIKE 'lockguard_%' ORDER BY pid"
        exit 1
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
        fail "timeout: $2"
      }

      stmt() { $LG_GATE --print "$1"; }

      # The registry's text with its placeholders bound to literals: the phase runs the
      # statements the server runs, not a copy of them.
      bind() {
        s="$1"
        s="''${s//\$1/$2}"
        if [ $# -ge 3 ]; then s="''${s//\$2/$3}"; fi
        printf '%s;' "$s"
      }

      LOCK_ANCHORS=$(stmt LOCK_ANCHORS) || exit 1
      LOCK_SEED=$(stmt LOCK_SEED_ANCHORS) || exit 1
      WHOLE_AMONG=$(stmt WHOLE_AMONG) || exit 1
      LOCK_PATHS=$(stmt LOCK_CACHED_PATHS) || exit 1
      SEED=$(stmt SEED_MISSING_RUNTIME_DEPS) || exit 1
      DEPENDENTS=$(stmt RUNTIME_DEPENDENT_COUNTS) || exit 1
      COUNT_DOWN=$(stmt COUNT_DOWN_RUNTIME) || exit 1
      COUNT_UP=$(stmt COUNT_UP_RUNTIME) || exit 1
      RECOUNT=$(stmt RECOUNT_MISSING_RUNTIME_DEPS) || exit 1

      send_a() { printf '%s\n' "$1" >&3; }
      send_b() { printf '%s\n' "$1" >&4; }
      idle_tx() { wait_state "application_name = 'lockguard_$1' AND state = 'idle in transaction'" "$2"; }
      idle() { wait_state "application_name = 'lockguard_$1' AND state = 'idle'" "$2"; }
      blocked() { wait_state "application_name = 'lockguard_$1' AND wait_event_type = 'Lock'" "$2"; }

      # P, D and Q each have one output. P's and Q's are in the cache; D's is when
      # `$1` says so. No edges: each arm adds the ones it races on.
      reset() {
        q "TRUNCATE derivation_build, derivation_dependency, derivation_output, cached_path;
           INSERT INTO derivation_build (id, derivation, created_at, updated_at)
             SELECT uuidv7(), d, now(), now() FROM unnest(ARRAY['$P', '$DEP', '$Q']::uuid[]) d;
           INSERT INTO derivation_output (id, derivation, name, hash, package, created_at)
             VALUES (uuidv7(), '$P', 'out', 'lgp', 'p', now()),
                    (uuidv7(), '$DEP', 'out', 'lgd', 'd', now()),
                    (uuidv7(), '$Q', 'out', 'lgq', 'q', now());
           INSERT INTO cached_path (id, hash, package, file_hash, created_at)
             VALUES (uuidv7(), 'lgp', 'p', 'sha256:p', now()), (uuidv7(), 'lgq', 'q', 'sha256:q', now());" >/dev/null
        if [ "$1" = present ]; then
          q "INSERT INTO cached_path (id, hash, package, file_hash, created_at) VALUES (uuidv7(), 'lgd', 'd', 'sha256:d', now());" >/dev/null
        fi
        : > $D/a.out
        : > $D/b.out
      }

      recount() {
        n=$(printf '%s\n' "SET search_path = lockguard, public;" "$RECOUNT;" | pg | grep '^UPDATE' | awk '{print $2}')
        if grep -q ERROR $D/a.out $D/b.out; then fail "$1: a session reported an error"; fi
        echo "lockguard $1: recount wrote $n"
      }

      rm -rf $D
      mkdir -p $D
      q "DROP SCHEMA IF EXISTS lockguard CASCADE; CREATE SCHEMA lockguard;
         CREATE TABLE lockguard.derivation_build (LIKE public.derivation_build INCLUDING DEFAULTS INCLUDING INDEXES);
         CREATE TABLE lockguard.derivation_dependency (LIKE public.derivation_dependency INCLUDING DEFAULTS INCLUDING INDEXES);
         CREATE TABLE lockguard.derivation_output (LIKE public.derivation_output INCLUDING DEFAULTS INCLUDING INDEXES);
         CREATE TABLE lockguard.cached_path (LIKE public.cached_path INCLUDING DEFAULTS INCLUDING INDEXES);" >/dev/null
      mkfifo $D/a.in
      mkfifo $D/b.in
      reset absent
      pg < $D/a.in >> $D/a.out 2>&1 &
      APID=$!
      pg < $D/b.in >> $D/b.out 2>&1 &
      BPID=$!
      exec 3> $D/a.in
      exec 4> $D/b.in
      send_a "SET application_name = 'lockguard_a'; SET search_path = lockguard, public;"
      send_b "SET application_name = 'lockguard_b'; SET search_path = lockguard, public;"

      # 1. The flip holds D first. The seed of P waits on D's key, so it counts D after
      # the flip committed and the ripple, which could not see P's edge, owes P nothing.
      send_b "BEGIN;"
      send_b "$(bind "$LOCK_ANCHORS" "'{$DEP}'")"
      send_b "INSERT INTO cached_path (id, hash, package, file_hash, created_at) VALUES (uuidv7(), 'lgd', 'd', 'sha256:d', now());"
      idle_tx b "flip-first: the flip never held D"
      send_a "BEGIN;"
      send_a "INSERT INTO derivation_dependency (derivation, dependency, kind) VALUES ('$P', '$DEP', 1);"
      send_a "$(bind "$LOCK_SEED" "'{$P}'")"
      blocked a "flip-first: the seed of P did not wait on D's key"
      send_b "$(bind "$SEED" "'{$DEP}'" "'{t}'")"
      send_b "$(bind "$DEPENDENTS" "'{$DEP}'")"
      send_b "COMMIT;"
      idle b "flip-first: the flip never committed"
      idle_tx a "flip-first: the seed never resumed"
      send_a "$(bind "$SEED" "'{$P}'" "'{f}'")"
      send_a "COMMIT;"
      idle a "flip-first: the seed never committed"
      recount flip-first

      # The same interleaving under the flip's own lock, which names no dependency: the
      # seed does not wait, counts D as a hole, and nothing ever counts it down. The
      # contrast is the assertion that the shared keys are what makes arm 1 right.
      reset absent
      send_b "BEGIN;"
      send_b "$(bind "$LOCK_ANCHORS" "'{$DEP}'")"
      send_b "INSERT INTO cached_path (id, hash, package, file_hash, created_at) VALUES (uuidv7(), 'lgd', 'd', 'sha256:d', now());"
      idle_tx b "unguarded: the flip never held D"
      send_a "BEGIN;"
      send_a "INSERT INTO derivation_dependency (derivation, dependency, kind) VALUES ('$P', '$DEP', 1);"
      send_a "$(bind "$LOCK_ANCHORS" "'{$P}'")"
      send_a "$(bind "$SEED" "'{$P}'" "'{f}'")"
      idle_tx a "unguarded: the seed blocked without the shared keys"
      send_b "$(bind "$SEED" "'{$DEP}'" "'{t}'")"
      send_b "$(bind "$DEPENDENTS" "'{$DEP}'")"
      send_b "COMMIT;"
      idle b "unguarded: the flip never committed"
      send_a "COMMIT;"
      idle a "unguarded: the seed never committed"
      recount unguarded

      # 2. The seed holds D's key first. The flip waits for it, then its ripple sees P's
      # committed edge and counts P down.
      reset absent
      send_a "BEGIN;"
      send_a "INSERT INTO derivation_dependency (derivation, dependency, kind) VALUES ('$P', '$DEP', 1);"
      send_a "$(bind "$LOCK_SEED" "'{$P}'")"
      send_a "$(bind "$SEED" "'{$P}'" "'{f}'")"
      idle_tx a "seed-first: the seed never held D's key"
      send_b "BEGIN;"
      send_b "$(bind "$LOCK_ANCHORS" "'{$DEP}'")"
      blocked b "seed-first: the flip did not wait on the seed"
      send_a "COMMIT;"
      idle a "seed-first: the seed never committed"
      idle_tx b "seed-first: the flip never resumed"
      send_b "INSERT INTO cached_path (id, hash, package, file_hash, created_at) VALUES (uuidv7(), 'lgd', 'd', 'sha256:d', now());"
      send_b "$(bind "$SEED" "'{$DEP}'" "'{t}'")"
      send_b "$(bind "$DEPENDENTS" "'{$DEP}'")"
      send_b "$(bind "$COUNT_DOWN" "'{$P}'" "'{1}'")"
      send_b "COMMIT;"
      idle b "seed-first: the flip never committed"
      grep -q "^$P|1$" $D/b.out || fail "seed-first: the ripple did not see P's edge"
      recount seed-first

      # 3. A retire of D against a seed of a second referrer Q: the retire's opening lock
      # waits for Q's seed on D's key, and its count-up then reaches both referrers.
      reset present
      q "INSERT INTO derivation_dependency (derivation, dependency, kind) VALUES ('$P', '$DEP', 1);" >/dev/null
      send_a "BEGIN;"
      send_a "INSERT INTO derivation_dependency (derivation, dependency, kind) VALUES ('$Q', '$DEP', 1);"
      send_a "$(bind "$LOCK_SEED" "'{$Q}'")"
      send_a "$(bind "$SEED" "'{$Q}'" "'{f}'")"
      idle_tx a "unwhole: the seed of Q never held D's key"
      send_b "BEGIN;"
      send_b "$(bind "$LOCK_PATHS" "'{lgd}'")"
      blocked b "unwhole: the retire did not wait on the seed"
      send_a "COMMIT;"
      idle a "unwhole: the seed never committed"
      idle_tx b "unwhole: the retire never resumed"
      send_b "$(bind "$WHOLE_AMONG" "'{$DEP}'")"
      send_b "DELETE FROM cached_path WHERE hash = 'lgd';"
      send_b "$(bind "$DEPENDENTS" "'{$DEP}'")"
      send_b "$(bind "$COUNT_UP" "'{$P,$Q}'" "'{1,1}'")"
      send_b "COMMIT;"
      idle b "unwhole: the retire never committed"
      grep -q "^$Q|1$" $D/b.out || fail "unwhole: the ripple did not see Q's edge"
      recount unwhole

      # 3b. A retire that waited on a flip reads the flipped row. The flip counts D down
      # to whole under D's key; the retire's opening lock waits on that key, so the
      # statement that reads whether D was whole starts after the flip committed.
      reset present
      q "UPDATE derivation_build SET missing_runtime_deps = 1 WHERE derivation = '$DEP';" >/dev/null
      send_a "BEGIN;"
      send_a "$(bind "$LOCK_ANCHORS" "'{$DEP}'")"
      send_a "$(bind "$COUNT_DOWN" "'{$DEP}'" "'{1}'")"
      idle_tx a "retire-reads: the flip never held D"
      send_b "BEGIN;"
      send_b "$(bind "$LOCK_PATHS" "'{lgd}'")"
      blocked b "retire-reads: the retire did not wait on the flip"
      send_a "COMMIT;"
      idle a "retire-reads: the flip never committed"
      idle_tx b "retire-reads: the retire never resumed"
      send_b "$(bind "$WHOLE_AMONG" "'{$DEP}'")"
      send_b "ROLLBACK;"
      idle b "retire-reads: the retire never finished"
      grep -q "^$DEP$" $D/b.out || fail "retire-reads: the retire read D as it was before the flip it waited for"
      recount retire-reads

      # 4. Two seeds sharing D hold its key shared, and neither waits for the other.
      reset present
      send_a "BEGIN;"
      send_a "INSERT INTO derivation_dependency (derivation, dependency, kind) VALUES ('$P', '$DEP', 1);"
      send_a "$(bind "$LOCK_SEED" "'{$P}'")"
      idle_tx a "shared: the first seed never held D's key"
      send_b "BEGIN;"
      send_b "INSERT INTO derivation_dependency (derivation, dependency, kind) VALUES ('$Q', '$DEP', 1);"
      send_b "$(bind "$LOCK_SEED" "'{$Q}'")"
      idle_tx b "shared: the second seed waited on the first"
      send_a "$(bind "$SEED" "'{$P}'" "'{f}'")"
      send_b "$(bind "$SEED" "'{$Q}'" "'{f}'")"
      send_a "COMMIT;"
      send_b "COMMIT;"
      idle a "shared: the first seed never committed"
      idle b "shared: the second seed never committed"
      recount shared

      q "DROP SCHEMA lockguard CASCADE;" >/dev/null
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
      # The lock's narHash is the input's own, substituted rather than recomputed:
      # `nix hash path` reads all of nixpkgs, and that I/O storm starved Postgres
      # for a minute, long enough for every pool in the server to time out.
      server.succeed("sed -i 's#\\[hash\\]#${self.inputs.nixpkgs.narHash}#g' /var/lib/git/test/flake.lock")

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
              # A stalled run reads the same on the eval and the builds whether the
              # graph is wedged or the fleet has left, and those want opposite fixes.
              fleet = server.succeed(
                  f'{CURL} -sf -H "Authorization: Bearer {token}" '
                  f'{API}/board/health | '
                  f'{JQ} -c ".message | {{workers: .workers_connected, sessions: .proto_sessions, '
                  f'pending: .jobs_pending, active: .jobs_active}}"'
              ).strip()
              print(
                  f"  [{attempt:>2}/90] eval={eval_detail} builds={builds_summary} "
                  f"fleet={fleet}"
              )

      if not completed:
          # A stall is an anchor that never went terminal, so the anchor states are the
          # diagnosis and the journal is the supporting evidence. The histogram gives
          # the shape; the gate buckets name which of `gates_predicate`'s terms is
          # false for the anchors the evaluation is still waiting on, which is the one
          # thing the shape cannot tell apart.
          eval_state = sql(
              f"SELECT status::text || ' since ' || updated_at::text"
              f" FROM evaluation WHERE id = '{eval_id}';"
          )
          anchors = sql(
              f"SELECT db.status::text || ' fetchable=' || db.fetchable::int::text"
              f"  || ' demanded=' || db.demanded::int::text"
              f"  || ' unready_deps=' || db.unready_deps || ' count=' || count(*)::text"
              f" FROM derivation_build db"
              f" JOIN build_job bj ON bj.derivation_build = db.id"
              f" WHERE bj.evaluation = '{eval_id}'"
              f" GROUP BY db.status, db.fetchable, db.demanded, db.unready_deps ORDER BY 1;"
          )
          gates = blocking_anchors(eval_id)
          # The journal tail was 80 lines of which 60 were `check_task_updates`,
          # whose task Model prints 1.5 KB per line, so the polling and GC chatter
          # goes. A stall is usually a worker that left or an evaluation whose
          # worker did, and both are minutes old by the time the deadline fires -
          # hence the session events over the whole window, and both workers' own
          # journals, which the server's says nothing about.
          noise = (
              "gradient_web: (request started|response generated"
              "|sending chunk|stream closed)"
              "|gradient_sources::git::(update_check|commit_info|pktline)"
              "|gradient_cache::cacher: (Cache cleanup|Evaluation GC|Derivation GC)"
              "|handler::dispatch: WorkerMetrics"
          )
          j = server.succeed(
              f"journalctl -u gradient-server --no-pager --since='-900s' -n 8000"
              f" | grep -vE '{noise}' | tail -n 60"
          )
          fleet = server.succeed(
              "journalctl -u gradient-server --no-pager --since='-900s' -n 8000"
              " | grep -E 'handshake complete|duplicate connection|worker silent"
              "|unregister|job accepted|job rejected|abandoned|not streaming'"
              " | tail -n 40"
          )
          workers = "".join(
              f"\n{name}:\n"
              + node.succeed(
                  "journalctl -u gradient-worker --no-pager --since='-300s' -n 400"
                  " | tail -n 40"
              )
              for name, node in (("builder", builder), ("builder2", builder2))
          )
          raise Exception(
              f"Evaluation did not complete after 900 s. Evaluation is {eval_state}.\n"
              f"anchors of this evaluation:\n{anchors}\n"
              f"what the evaluation still waits on, by gate:\n{gates}\n"
              f"fleet events over the window:\n{fleet}\n"
              f"worker journals:{workers}\n"
              f"server log, polling and GC noise removed:\n{j}"
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
      # identical build closures, no anchor left without its edges, and no pool
      # exhaustion or dropped call in the server log. A walk prunes whatever the
      # other worker's batch recorded first and names only the pruned root, so
      # the two `build_job` sets agree on the closure they reach, not on their size.
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

      def reach(name, evaluation):
          return (
              f"{name}(derivation) AS ("
              f"  SELECT derivation FROM build_job WHERE evaluation = '{evaluation}'"
              f"  UNION SELECT e.dependency FROM derivation_dependency e "
              f"  JOIN {name} c ON e.derivation = c.derivation)"
          )

      reach1 = reach("reach1", eval_id)
      reach2 = reach("reach2", eval2_id)
      builds1 = int(sql(f"SELECT count(*) FROM build_job WHERE evaluation = '{eval_id}';"))
      builds2 = int(sql(f"SELECT count(*) FROM build_job WHERE evaluation = '{eval2_id}';"))
      closure1 = int(sql(f"WITH RECURSIVE {reach1} SELECT count(*) FROM reach1;"))
      closure2 = int(sql(f"WITH RECURSIVE {reach2} SELECT count(*) FROM reach2;"))
      print(f"build jobs: eval1={builds1} eval2={builds2}; closures: eval1={closure1} eval2={closure2}")
      assert builds1 > 0 and builds2 > 0, "both evaluations record builds"
      assert closure1 > 0 and closure1 == closure2, "both evaluations reach the same number of derivations"
      differ = int(sql(
          f"WITH RECURSIVE {reach1}, {reach2} "
          f"SELECT (SELECT count(*) FROM (SELECT derivation FROM reach1 "
          f"        EXCEPT SELECT derivation FROM reach2) a) "
          f"     + (SELECT count(*) FROM (SELECT derivation FROM reach2 "
          f"        EXCEPT SELECT derivation FROM reach1) b);"
      ))
      assert differ == 0, f"the two evaluations' closures differ on {differ} derivations"
      unnamed = int(sql(
          f"WITH RECURSIVE {reach1} "
          f"SELECT count(*) FROM reach1 r WHERE NOT EXISTS ("
          f"  SELECT 1 FROM build_job bj WHERE bj.derivation = r.derivation "
          f"  AND bj.evaluation IN ('{eval_id}', '{eval2_id}'));"
      ))
      assert unnamed == 0, f"{unnamed} derivations of the closure were named by neither walk"
      duplicates = int(sql("SELECT count(*) - count(DISTINCT hash) FROM derivation;"))
      assert duplicates == 0, f"{duplicates} duplicate derivation rows"
      unwalked = int(sql(
          f"WITH RECURSIVE {reach1} "
          f"SELECT count(*) FROM reach1 r JOIN derivation d ON d.id = r.derivation WHERE NOT d.walked;"
      ))
      assert unwalked == 0, f"{unwalked} derivations of the closure are stubs"
      edges = int(sql("SELECT count(*) FROM derivation_dependency;"))
      assert edges > 0, "the graph recorded no dependency edge at all"
      unwalked_deps = int(sql(
          "SELECT count(*) FROM derivation_dependency e JOIN derivation d ON d.id = e.dependency "
          "WHERE NOT d.walked;"
      ))
      assert unwalked_deps == 0, f"{unwalked_deps} of {edges} dependency edges point at a stub"
      # The value, not just the count: a NEGATIVE counter is a ripple that moved a
      # row no seed had counted, and it never reads `= 0` again, so the prune stops
      # for good. A positive one is a seed that missed a count-down.
      incomplete = sql(
          "SELECT d.name || ' unwalked_inputs=' || d.unwalked_inputs::text"
          " FROM derivation d WHERE d.walked AND d.unwalked_inputs <> 0"
          " ORDER BY d.unwalked_inputs, d.name LIMIT 20;"
      )
      assert not incomplete, (
          f"walked derivations still count an unwalked input after a complete walk:\n{incomplete}"
      )
      assert_no_server_error(server.succeed("journalctl -u gradient-server --no-pager"))

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
      have_runtime_index = int(sql(
          "SELECT count(*) FROM pg_indexes "
          "WHERE indexname = 'idx-derivation_dependency-runtime';"
      ))
      assert have_runtime_index == 1, "the runtime walk lost its covering index"
      leftover_keys = int(sql(
          "SELECT count(*) FROM information_schema.columns "
          "WHERE column_name = 'id' AND table_name = 'derivation_dependency';"
      ))
      assert leftover_keys == 0, f"{leftover_keys} junction tables kept a surrogate key"
      retired_index = int(sql(
          "SELECT count(*) FROM information_schema.tables "
          "WHERE table_name = 'cached_path_reference';"
      ))
      assert retired_index == 0, "the path-level reference index outlived its migration"

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

          # The plan shape of this walk (a nested loop, never a merge join) is
          # asserted for every registered walk by the SQL plan gate in phase 13.

      # The table is gone; the task page fills the histogram cache for the
      # page it reads and stamps every entry point with the graph version.
      gone = int(sql(
          "SELECT count(*) FROM information_schema.tables "
          "WHERE table_name = 'derivation_closure';"
      ))
      assert gone == 0, "derivation_closure is still there"

      # Give the evaluation one entry point with no build_job, so the predicate
      # below has something to exclude: without it this row is unstamped forever
      # and the assertion fires, and if the endpoint counted it, total would
      # exceed the page. Nothing in a real evaluation produces such a row, which
      # is why it has to be made here.
      sql(
          f"WITH d AS ("
          f"  INSERT INTO derivation (id, hash, name, architecture, created_at) "
          f"  VALUES (uuidv7(), 'zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz', 'unreportable', "
          f"          'x86_64-linux', now() AT TIME ZONE 'UTC') RETURNING id) "
          f"INSERT INTO entry_point (id, task, evaluation, derivation, eval, created_at) "
          f"SELECT uuidv7(), ep.task, ep.evaluation, d.id, 'zz.unreportable', "
          f"       now() AT TIME ZONE 'UTC' "
          f"FROM d, entry_point ep WHERE ep.evaluation = '{eval_id}' LIMIT 1;"
      )
      planted = int(sql(
          f"SELECT count(*) FROM entry_point WHERE evaluation = '{eval_id}' "
          f"AND eval = 'zz.unreportable';"
      ))
      assert planted == 1, "the unreportable entry point was not planted"

      # An entry point with no build_job in this evaluation is not reportable, so
      # the page never covers it and nothing ever stamps it.
      reportable = (
          f"ep.evaluation = '{eval_id}' AND EXISTS ("
          f"  SELECT 1 FROM build_job bj WHERE bj.evaluation = '{eval_id}'"
          f"  AND bj.derivation = ep.derivation)"
      )
      # Read the version BEFORE the page. The stamp is monotone and is at least the
      # version the reader saw, so asserting against that floor is race-free, while
      # comparing with the version afterwards loses to any concurrent anchor move.
      version_before = int(sql(
          f"SELECT graph_version FROM evaluation WHERE id = '{eval_id}';"
      ))
      page = json.loads(api_get(
          token, f"tasks/project/task/entry-points?evaluation_id={eval_id}&limit=500"
      ))["message"]
      assert page["total"] == len(page["entry_points"]) > 0, page
      unstamped = int(sql(
          f"SELECT count(*) FROM entry_point ep WHERE {reportable} "
          f"AND (ep.dep_counts_version IS NULL "
          f"     OR ep.dep_counts_version < {version_before});"
      ))
      assert unstamped == 0, f"{unstamped} entry points were not stamped by the read"

      # What the read stored must be what the root-attributed fenced walk says.
      drift = int(sql(
          f"WITH RECURSIVE stored AS ("
          f"  SELECT ep.id, coalesce(sum(c.count), 0) AS total FROM entry_point ep "
          f"  LEFT JOIN entry_point_dep_count c ON c.entry_point = ep.id "
          f"  WHERE {reportable} GROUP BY ep.id), "
          f"closure(ep, drv) AS ("
          f"  SELECT ep.id, ep.derivation FROM entry_point ep WHERE {reportable} "
          f"  UNION SELECT c.ep, s.next FROM closure c, LATERAL ("
          f"    SELECT dd.dependency AS next FROM derivation_dependency dd "
          f"    JOIN build_job bj ON bj.derivation = dd.dependency AND bj.evaluation = '{eval_id}' "
          f"    WHERE dd.derivation = c.drv OFFSET 0) s), "
          f"live AS ("
          f"  SELECT ep.id, count(*) AS total FROM entry_point ep "
          f"  JOIN closure c ON c.ep = ep.id AND c.drv <> ep.derivation "
          f"  JOIN build_job bj ON bj.derivation = c.drv AND bj.evaluation = '{eval_id}' "
          f"  WHERE {reportable} GROUP BY ep.id) "
          f"SELECT count(*) FROM live l JOIN stored s ON s.id = l.id WHERE l.total <> s.total;"
      ))
      assert drift == 0, f"{drift} entry points disagree with the live walk"
      by_id = {ep["id"]: ep for ep in page["entry_points"]}
      stored_totals = sql(
          f"SELECT ep.id || ':' || coalesce(sum(c.count), 0) FROM entry_point ep "
          f"LEFT JOIN entry_point_dep_count c ON c.entry_point = ep.id "
          f"WHERE {reportable} GROUP BY ep.id;"
      ).split()
      for row in stored_totals:
          ep_id, total = row.split(":")
          assert by_id[ep_id]["deps_total"] == int(total), f"{ep_id}: api {by_id[ep_id]['deps_total']} stored {total}"

      # The planted row has served its purpose, and `GET /evals/{id}` lists entry
      # points unfiltered, so leaving it would report `zz.unreportable` as Queued
      # for every later phase.
      sql(
          f"DELETE FROM entry_point WHERE evaluation = '{eval_id}' "
          f"AND eval = 'zz.unreportable';"
      )
      sql("DELETE FROM derivation WHERE hash = 'zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz';")
      left = int(sql(
          f"SELECT count(*) FROM entry_point WHERE evaluation = '{eval_id}' "
          f"AND eval = 'zz.unreportable';"
      ))
      assert left == 0, "the unreportable entry point outlived its assertions"

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
      # graph must record exactly the input drvs the `.drv` itself declares. Only
      # the BUILD edges: since #671 the same relation carries the runtime graph,
      # whose edges are learned from the NAR and reach producers that are not
      # direct inputs at all (hello references glibc, which stdenv brings in).
      drv_hash = store_path_drv.split("/")[-1].split("-")[0]
      declared = int(builder.succeed(
          f"{NIX} derivation show {store_path_drv} --extra-experimental-features nix-command "
          f"| {JQ} '[.derivations[] | (.inputDrvs // .inputs.drvs // {{}}) | length] | add'"
      ).strip())
      recorded = int(sql(
          f"SELECT count(*) FROM derivation_dependency e JOIN derivation d ON d.id = e.derivation "
          f"WHERE d.hash = '{drv_hash}' AND e.kind IN (0, 2);"
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
      # row and ripples the loss up to every anchor that trusted it, a re-upload
      # seeds it whole again and ripples that back. `missing_runtime_deps` is
      # moved, never re-derived, so the recompute has to agree at every step.
      banner("Phase 10c: missing_runtime_deps moves on retire and re-upload")

      retired_columns = int(sql(
          "SELECT count(*) FROM information_schema.columns "
          "WHERE table_name = 'cached_path' "
          "  AND column_name IN ('closure_complete', 'missing_references');"
      ))
      assert retired_columns == 0, "cached_path still carries a retired wholeness column"

      # Phase 10d bills this cycle, so open the accounting before it runs. The
      # library counts from server start; the extension only exposes the view.
      sql("CREATE EXTENSION IF NOT EXISTS pg_stat_statements;")
      COUNTER_WRITES = "s.query ILIKE '%update derivation_build%missing_runtime_deps%'"
      counter_rows_before = int(sql(
          f"SELECT coalesce(sum(s.rows), 0) FROM pg_stat_statements s WHERE {COUNTER_WRITES};"
      ))

      # The anchor side of the same idea (#591): both readiness columns are moved
      # by the event that changes them, so a recompute has to agree with every row.
      # The `derivation_output` guard is not a tautology - `NOT EXISTS` is vacuous
      # for an anchor with no output rows, and without it every output-less
      # terminal-success anchor reads as fetchable, which is the unbacked-output
      # dead zone. The dependency count LEFT JOINs for the same reason the gate
      # does: a dependency with no anchor row at all counts as unready. Since #593
      # an upstream copy is NOT fetchable: a dependent waits for the relay.
      def anchor_drift():
          return int(sql(
              "SELECT count(*) FROM derivation_build db WHERE db.unready_deps <> ("
              "  SELECT count(*) FROM derivation_dependency e "
              "  LEFT JOIN derivation_build dep ON dep.derivation = e.dependency "
              "  WHERE e.derivation = db.derivation "
              "    AND (dep.derivation IS NULL OR NOT dep.fetchable)) "
              "OR db.fetchable <> (db.status IN (3, 7) AND db.missing_runtime_deps = 0 "
              "AND EXISTS ("
              "  SELECT 1 FROM derivation_output o2 WHERE o2.derivation = db.derivation) "
              "AND NOT EXISTS ("
              "  SELECT 1 FROM derivation_output o LEFT JOIN cached_path cp ON cp.hash = o.hash "
              "  WHERE o.derivation = db.derivation AND cp.file_hash IS NULL));"
          ))

      # The second readiness counter, one per edge kind (#671). Wholeness moved from
      # the path to the anchor: an anchor is whole when every output is present and
      # no runtime edge leads to something that is not, so the recompute is a walk up
      # from what is not present and has to agree with every stored count.
      def runtime_drift():
          return int(sql(
              "WITH RECURSIVE unwhole(derivation) AS ("
              "  SELECT db.derivation FROM derivation_build db WHERE NOT ("
              "    EXISTS (SELECT 1 FROM derivation_output o2 WHERE o2.derivation = db.derivation) "
              "    AND NOT EXISTS (SELECT 1 FROM derivation_output o "
              "                    LEFT JOIN cached_path cp ON cp.hash = o.hash "
              "                    WHERE o.derivation = db.derivation AND cp.file_hash IS NULL)) "
              "  UNION "
              "  SELECT e.derivation FROM derivation_dependency e "
              "  JOIN unwhole u ON u.derivation = e.dependency WHERE e.kind IN (1, 2)) "
              "SELECT count(*) FROM derivation_build db WHERE db.missing_runtime_deps <> ("
              "  SELECT count(*) FROM derivation_dependency e "
              "  JOIN unwhole u ON u.derivation = e.dependency "
              "  WHERE e.derivation = db.derivation AND e.kind IN (1, 2));"
          ))

      def anchor_whole(drv):
          return sql(
              f"SELECT db.missing_runtime_deps::text || ' ' || db.fetchable::int::text "
              f"FROM derivation_build db JOIN derivation d ON d.id = db.derivation "
              f"WHERE d.hash = '{drv}';"
          ).split()

      # The third counter (#666). Demand is reachability from the open entry points
      # through open anchors, so the recompute is a walk and not a per-row subquery:
      # a builder steps over every edge, anything else over its runtime edges, and
      # an anchor is reached while it is open (not fetchable, not the requeue's). A
      # relay is reached and never stepped through on a build edge, which is the
      # whole reason a relayed subtree stops being built; a `Completed` anchor with
      # a hole in its closure is open, which is how the hole is reached. Open
      # anchors only: a settled one keeps whatever it carried and nothing reads it.
      def demand_drift():
          def is_open(a):
              return f"NOT {a}.fetchable AND {a}.status NOT IN (4, 6, 9)"
          def is_builder(a):
              return f"w.walked AND {a}.probed AND NOT {a}.substitutable AND {a}.status IN (0, 1, 2, 8)"
          return int(sql(
              "WITH RECURSIVE demanded(derivation, builder) AS ("
              f"  SELECT db.derivation, ({is_builder('db')}) FROM entry_point ep "
              "  JOIN derivation_build db ON db.derivation = ep.derivation "
              f"  JOIN derivation w ON w.id = db.derivation WHERE {is_open('db')} "
              "  UNION "
              f"  SELECT e.dependency, ({is_builder('dep')}) FROM demanded c "
              "  JOIN derivation_dependency e ON e.derivation = c.derivation "
              "  JOIN derivation_build dep ON dep.derivation = e.dependency "
              "  JOIN derivation w ON w.id = dep.derivation "
              f"  WHERE (c.builder OR e.kind IN (1, 2)) AND {is_open('dep')}) "
              f"SELECT count(*) FROM derivation_build db WHERE {is_open('db')} "
              "AND db.demanded <> (db.derivation IN (SELECT derivation FROM demanded));"
          ))

      # The sweep counts this and repairs nothing: a terminal-success producer whose
      # output no artifact backs is never fetchable, so every dependent of it waits
      # for an event that cannot come. Two of these wedged an evaluation for 900 s.
      def unbacked():
          return int(sql(
              "SELECT count(DISTINCT o.hash) FROM derivation_output o "
              "JOIN derivation_build db ON db.derivation = o.derivation "
              "WHERE db.status IN (3, 7) AND o.external_url IS NULL "
              "  AND NOT EXISTS (SELECT 1 FROM cached_path cp "
              "                  WHERE cp.hash = o.hash AND cp.file_hash IS NOT NULL);"
          ))

      def poll(query, want, what, timeout=180):
          for _ in range(timeout):
              if sql(query) == want:
                  return
              server.sleep(1)
          raise Exception(f"{what} (still {sql(query)!r}, want {want!r})")

      assert runtime_drift() == 0, "wholeness disagrees with its recompute before the retire"
      assert anchor_drift() == 0, "anchor counters disagree with their recompute before the retire"
      assert unbacked() == 0, "a producer this build settled has an output nothing backs"
      assert anchor_whole(drv_hash)[0] == "0", "hello's anchor is not whole to start with"

      # glibc first, since hello links against it.
      refs = sql(
          f"SELECT cp.hash || '-' || cp.package FROM cached_path cp "
          f"WHERE cp.hash IN (SELECT split_part(t.tok, '-', 1) FROM cached_path r, "
          f"    unnest(string_to_array(r.\"references\", ' ')) AS t(tok) "
          f"    WHERE r.hash = '{store_hash}' AND length(t.tok) > 0) "
          f"  AND cp.hash <> '{store_hash}' AND cp.file_hash IS NOT NULL "
          f"ORDER BY (cp.package LIKE 'glibc-%') DESC, cp.package;"
      ).splitlines()

      # `NarStore` shards its objects by the store hash, under the module's baseDir.
      def nar_object(name):
          h = name.split("-")[0]
          return f"/var/lib/gradient/nars/{h[:2]}/{h[2:]}.nar.zst"

      # The path has to be in the server's own store: the re-upload below runs the CLI
      # there. hello's only runtime reference is glibc, so there is no second candidate
      # to fall back on and the precondition has to be made rather than looked for.
      dep_name = next(
          (n for n in (name.strip() for name in refs)
           if n and server.execute(f"test -e /nix/store/{n}")[0] == 0),
          None,
      )
      assert dep_name, f"none of hello's {len(refs)} whole references is in the server store"
      dep_path = f"/nix/store/{dep_name}"
      dep_hash = dep_name.split("-")[0]
      dep_object = nar_object(dep_name)

      # A whole `cached_path` row does not imply a local object: the row can be recorded
      # from an upstream narinfo, or outlive its object until the zombie purge catches
      # up. Retiring needs something to delete, so push it first when it is not there.
      if server.execute(f"test -f {dep_object}")[0] != 0:
          print(f"{dep_path} is whole in the index with no local object; pushing it first")
          server.succeed(f"{CLI} cache upload main {dep_path}")
      server.succeed(f"test -f {dep_object}")

      # The purge below takes every row whose object is gone, not just the one this
      # phase deletes, and phase 10e's settle from cache needs EVERY output of the
      # victim's producer to still have a row. A sibling output that is already a
      # zombie would be swept up with the victim and is not restored by the single
      # re-upload, so back them all on disk while there is still something to push.
      siblings = sql(
          f"SELECT cp.hash || '-' || cp.package FROM derivation_output o "
          f"JOIN cached_path cp ON cp.hash = o.hash "
          f"WHERE o.derivation = (SELECT derivation FROM derivation_output "
          f"                      WHERE hash = '{dep_hash}') AND o.hash <> '{dep_hash}';"
      ).splitlines()
      for sibling in (s.strip() for s in siblings):
          if not sibling or server.execute(f"test -f {nar_object(sibling)}")[0] == 0:
              continue
          if server.execute(f"nix-store --check-validity /nix/store/{sibling} >/dev/null 2>&1")[0] != 0:
              print(f"{sibling} shares the producer, has no object and the store cannot realize its path; "
                    f"the purge will take it too")
              continue
          print(f"{sibling} shares the victim's producer with no object; pushing it first")
          server.succeed(f"{CLI} cache upload main /nix/store/{sibling}")

      print(f"retiring {dep_path}")
      indexed_before = set(sql("SELECT hash FROM cached_path;").split())
      server.succeed(f"rm {dep_object}")

      # The zombie purge is the retiring caller here. The deep GC runs that same
      # pass now instead of waiting out cacheMaintenanceIntervalSecs.
      server.succeed(
          f"{CURL} -sf -X POST -H 'Authorization: Bearer {token}' "
          f"{API}/admin/maintenance/deep-gc"
      )
      poll(f"SELECT count(*) FROM cached_path WHERE hash = '{dep_hash}';", "0",
           "the zombie purge kept a row whose NAR is gone")
      purged = indexed_before - set(sql("SELECT hash FROM cached_path;").split())
      assert dep_hash in purged, f"the purge took {sorted(purged)}, not the victim {dep_hash}"
      bystanders = sorted(purged - set([dep_hash]))
      if bystanders:
          print(f"the purge also took {len(bystanders)} rows that were already zombies: {bystanders}")

      assert runtime_drift() == 0, "wholeness disagrees with its recompute after the retire"
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

      # The same fact on the anchor. hello references the retired path, so the
      # runtime edge to its producer is a hole and hello stops being whole; the
      # counter is moved by the retire's ripple, never re-derived.
      hello_missing, hello_fetchable = anchor_whole(drv_hash)
      assert int(hello_missing) >= 1, (
          f"hello's anchor must count the retired reference as a missing runtime dep, "
          f"has {hello_missing}")
      assert hello_fetchable == "0", "an anchor that is not whole must not be fetchable"

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
      assert runtime_drift() == 0, "wholeness disagrees with its recompute after the re-upload"
      assert anchor_drift() == 0, "anchor counters disagree with their recompute after the re-upload"
      poll(f"SELECT missing_runtime_deps FROM derivation_build db "
           f"JOIN derivation d ON d.id = db.derivation WHERE d.hash = '{drv_hash}';", "0",
           "the re-upload did not ripple hello's anchor back to whole")

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
      assert runtime_drift() == 0, "wholeness disagrees with its recompute after the self-heal"
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
      anchors = int(sql("SELECT count(*) FROM derivation_build;"))
      assert 0 < counter_rows <= anchors, (
          f"one retire and one re-upload wrote {counter_rows} counter rows over a "
          f"{anchors}-anchor graph; a moved counter touches dependents, a derived one the table"
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
      print(f"server database time: {total_ms} ms, wholeness counter writes: {counter_share}%")
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
      for attempt in range(1, 91):
          server.sleep(10)
          candidate = server.succeed(
              f'{CURL} -sf -H "Authorization: Bearer {token}" '
              f'{API}/tasks/project/task | {JQ} -rj ".message.last_evaluation // empty"'
          ).strip()
          status3 = ""
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
          if attempt % 3 == 0:
              print(f"  [{attempt:>2}/90] re-eval candidate={candidate or 'none'} "
                    f"status={status3 or '-'}")
      if not eval3_id:
          # An ACTIVE `last_evaluation` blocks every later trigger for good -
          # `update_check` skips while it is - so a re-evaluation that never starts
          # and one that never finishes read the same from here. The evaluations and
          # the trigger's own decisions are what tell them apart.
          evals = sql(
              "SELECT e.id::text || ' status=' || e.status::text"
              " || ' created=' || e.created_at::text"
              " FROM evaluation e ORDER BY e.created_at DESC LIMIT 10;"
          )
          decided = server.succeed(
              "journalctl -u gradient-server --no-pager --since='-900s' -n 8000"
              " | grep -E 'skipping|update needed|Force evaluation|trigger created' "
              " | tail -n 15"
          )
          blocked = blocking_anchors(candidate) if candidate else "(no candidate)"
          raise Exception(
              f"the re-evaluation did not complete after 900 s. "
              f"task.last_evaluation={candidate or 'none'}, "
              f"eval1={eval_id}, eval2={eval2_id}\n"
              f"evaluations, newest first:\n{evals}\n"
              f"what it still waits on, by gate:\n{blocked}\n"
              f"what the trigger decided:\n{decided}"
          )

      # Polled, not sampled. The producer has no `build_job` of its own in an
      # evaluation that re-ingests nothing, so it is reached only through the eval
      # closure: the cache reconcile settles it where every output still has a row,
      # and otherwise the promotion queues a rebuild whose completion outlives the
      # evaluation. The evaluation reporting Completed says nothing about either.
      producer_state = (
          f"SELECT db.status::text || ' ' || db.fetchable::int::text FROM derivation_build db "
          f"JOIN derivation_output o ON o.derivation = db.derivation "
          f"WHERE o.hash = '{dep_hash}' LIMIT 1;"
      )
      for _ in range(60):
          producer = sql(producer_state)
          if producer in ("3 1", "7 1"):
              break
          server.sleep(10)
      else:
          unbacked = sql(
              f"SELECT count(*) FROM derivation_output o "
              f"LEFT JOIN cached_path cp ON cp.hash = o.hash "
              f"WHERE o.derivation = (SELECT derivation FROM derivation_output "
              f"                      WHERE hash = '{dep_hash}') AND cp.hash IS NULL;"
          )
          raise Exception(
              f"the retired output's producer never became terminal-success and fetchable "
              f"in 600 s, has (status fetchable) = ({producer}) with {unbacked} of its "
              f"outputs missing a cached_path row"
          )
      # The ripple decrements the dependents inside the same transaction that
      # flips the producer, so this is settled the moment the poll above sees it.
      # The short window is for a SECOND dependency still being re-pushed, and is
      # deliberately far inside the 300 s sweep: a ripple this missed must fail
      # here rather than be repaired into a pass.
      hello_unready = (
          f"SELECT db.unready_deps FROM derivation_build db "
          f"JOIN derivation d ON d.id = db.derivation WHERE d.hash = '{drv_hash}';"
      )
      for _ in range(12):
          if sql(hello_unready) == "0":
              break
          server.sleep(5)
      else:
          raise Exception(
              f"hello's counter must return to zero, is {sql(hello_unready)} with these "
              f"unfetchable inputs: " + sql(
                  f"SELECT string_agg(d.name || ' status=' || dep.status::text "
                  f"  || ' fetchable=' || dep.fetchable::int::text "
                  f"  || ' unready=' || dep.unready_deps::text, ', ') "
                  f"FROM derivation_dependency e "
                  f"JOIN derivation_build dep ON dep.derivation = e.dependency "
                  f"JOIN derivation d ON d.id = e.dependency "
                  f"WHERE e.derivation = (SELECT id FROM derivation WHERE hash = '{drv_hash}') "
                  f"  AND NOT dep.fetchable;"
              )
          )
      unsettled = int(sql(
          f"SELECT count(*) FROM build_job bj "
          f"JOIN derivation_build db ON db.derivation = bj.derivation "
          f"WHERE bj.evaluation = '{eval3_id}' AND (db.status NOT IN (3, 7) OR NOT db.fetchable);"
      ))
      assert unsettled == 0, f"{unsettled} anchors of the completed re-evaluation are not settled and fetchable"
      assert runtime_drift() == 0, "wholeness disagrees with its recompute after the re-evaluation"
      assert anchor_drift() == 0, "anchor counters disagree with their recompute after the re-evaluation"

      # Printed, not asserted: a whole cache needs no build, but the sweep above can
      # promote the same anchors inside this window and its dispatches are
      # indistinguishable from the evaluation's, so a zero here would be a coin flip.
      builds_after = int(sql("SELECT count(*) FROM dispatched_job WHERE kind = 1;"))
      print(f"builds dispatched around the re-evaluation: {builds_after - builds_before}")

      # A build-once anchor builds once (#654). Two successful builds of one
      # derivation is the most this run can legitimately want - its first, and the
      # one phase 10c's retire demoted it into - so a third means something re-armed
      # a build the graph had already got, which is how the unbacked-output loop
      # showed up: one dispatch per reconcile pass, rebuilding an output that never
      # came back. Outcomes 1 and 2 are `Built`/`Substituted`; a relay builds
      # nothing and is excluded.
      churn = sql(
          "SELECT string_agg(d.name || ' built ' || x.builds::text || ' times', ', ') "
          "FROM (SELECT ba.derivation_build, count(*) AS builds FROM build_attempt ba "
          "      WHERE NOT ba.substitute AND ba.outcome IN (1, 2) "
          "      GROUP BY ba.derivation_build HAVING count(*) > 2) x "
          "JOIN derivation_build db ON db.id = x.derivation_build "
          "JOIN derivation d ON d.id = db.derivation;"
      )
      assert churn == "", f"a build-once anchor was rebuilt past the one granted retry: {churn}"

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
              # Unconfirmed on purpose: no NAR backs these rows, and the cache cleanup
              # purges a CONFIRMED row whose object is gone, which took the second
              # fixture out from under the phase 20 seconds after it was written.
              f"INSERT INTO cached_path (id, hash, package, file_hash, file_size, nar_size, nar_hash, "
              f"confirmed, created_at) VALUES (uuidv7(), '{out_hash}', '{name}-out', "
              f"'sha256:lockrace', 1, 1, 'sha256:lockrace', false, now() AT TIME ZONE 'UTC');"
          )
          assert sql(
              f"SELECT db.fetchable::int::text || ' ' || (SELECT count(*)::text FROM cached_path "
              f"WHERE hash = '{out_hash}' AND file_hash IS NOT NULL) "
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
      race_drift = runtime_drift()
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
          f"(wholeness {race_drift}, readiness {race_anchor_drift})"
      )

      # ── Phase 10g: a relay happens when, and only when, something wants it ─
      # Half of all worker jobs used to be relays of outputs nothing had asked
      # for, and each one relayed the output ALONE: the members of its runtime
      # closure below a pruned node have no anchor of their own, so nothing ever
      # fetched them and every dependent's build fell back to the upstream. #593
      # is both halves - a substitutable anchor is queued only while an entry
      # point or a pending builder one hop above it demands it, and the relay
      # mirrors the whole closure so the output lands whole in our cache.
      #
      # busybox is the probe: served only by a file binary cache on this host,
      # wanted only by busywrap, which is built here. Every assertion is on the
      # database, because "was it relayed" is a `build_attempt` row and "did the
      # closure come with it" is the anchor's `missing_runtime_deps`.
      banner("Phase 10g: demand-driven substitution (#593)")

      # A narinfo whose Sig does not verify against the upstream's configured
      # public key is dropped, so the file cache is signed on the way out with
      # the key the cache declares. The upstream is declared rather than PUT:
      # `main` is state-managed, and every mutating cache endpoint refuses a
      # managed cache, so provisioning is the only way it can have one.
      server.succeed(
          f"{NIX} --extra-experimental-features 'nix-command flakes' copy "
          f"--to 'file:///srv/upstream?secret-key=/etc/gradient/secrets/upstream_key' --no-check-sigs "
          f"${pkgs.busybox.out} ${pkgs.busybox.debug}"
      )
      server.succeed("chown -R nginx:nginx /srv/upstream && systemctl reload nginx")
      server.succeed(f"{CURL} -sf http://gradient.local/upstream/nix-cache-info > /dev/null")
      # The relay downloads the upstream NAR from the BUILDER, so the upstream has
      # to answer there too. Asserted here because the alternative symptom is the
      # phase timing out 900 s later on an evaluation that never finishes.
      builder.succeed(f"{CURL} -sf http://server/upstream/nix-cache-info > /dev/null")

      assert "file-upstream" in api_get(token, "caches/main/upstreams"), "the declared upstream was not provisioned"

      def anchor_of(drv):
          """`<status> <substitutable> <relay attempts>` of `drv`'s anchor."""
          return sql(anchor_column(
              drv,
              "db.status::text || ' ' || db.substitutable::int::text || ' ' || "
              "(SELECT count(*) FROM build_attempt a "
              " WHERE a.derivation_build = db.id AND a.substitute)::text",
          ))

      def anchor_column(drv, column):
          return f"SELECT {column} FROM derivation_build db WHERE db.derivation = '{drv}';"

      def relay_attempts(drv):
          return anchor_column(
              drv,
              "(SELECT count(*) FROM build_attempt a "
              " WHERE a.derivation_build = db.id AND a.substitute)::text",
          )

      def output_missing(drv):
          """The anchor's runtime holes: zero means the whole relayed closure landed."""
          return sql(anchor_column(drv, "db.missing_runtime_deps::text"))

      def output_hash(drv):
          return sql(
              f"SELECT o.hash FROM derivation_output o "
              f"WHERE o.derivation = '{drv}' AND o.name = 'out';"
          )

      def wait_for_new_eval(known, timeout=900):
          """The id of the next evaluation to reach a terminal status."""
          candidate = ""
          for _ in range(timeout // 10):
              server.sleep(10)
              candidate = server.succeed(
                  f'{CURL} -sf -H "Authorization: Bearer {token}" '
                  f'{API}/tasks/project/task | {JQ} -rj ".message.last_evaluation // empty"'
              ).strip()
              if not candidate or candidate in known:
                  continue
              status = server.succeed(
                  f'{CURL} -sf -H "Authorization: Bearer {token}" '
                  f'{API}/evals/{candidate} | {JQ} -rj ".message.status"'
              ).strip()
              if status == "Completed":
                  return candidate
              if status in ("Failed", "Aborted"):
                  j = server.succeed("journalctl -u gradient-server --no-pager --since='-600s' -n 300")
                  raise Exception(f"evaluation {candidate} ended {status}:\n{j[-3000:]}")
          blocked = blocking_anchors(candidate) if candidate else "(no candidate)"
          edges = unready_reasons(candidate) if candidate else ""
          failed = sql(
              f"SELECT d.name || ' ' || db.status::text FROM derivation_build db"
              f" JOIN derivation d ON d.id = db.derivation"
              f" JOIN build_job bj ON bj.derivation_build = db.id"
              f" WHERE bj.evaluation = '{candidate}' AND db.status IN (4, 6, 9)"
              f" ORDER BY 1 LIMIT 40;"
          ) if candidate else ""
          raise Exception(
              f"no new evaluation completed within {timeout} s. candidate={candidate or 'none'}\n"
              f"what it still waits on, by gate:\n{blocked}\n"
              f"the edges those anchors count as unready:\n{edges}\n"
              f"terminal failures in the same evaluation:\n{failed}"
          )

      # busywrap links a binary out of busybox, so it needs busybox's output in
      # our cache before it can be built.
      server.succeed("cp /var/lib/git/flake-busywrap.nix /var/lib/git/test/flake.nix")
      server.succeed("sed -i 's#\\[nixpkgs\\]#${self.inputs.nixpkgs}#g' /var/lib/git/test/flake.nix")
      server.succeed(f"{GIT} -C /var/lib/git/test commit -am 'busywrap'")
      server.succeed("chown git:git -R /var/lib/git/test")
      eval4_id = wait_for_new_eval({eval_id, eval2_id, eval3_id})

      # Both probes name their derivation exactly: `LIKE 'busybox%'` also matches
      # the source tarball, a stub row written after the walked ones and so always
      # the newest, which is the one thing here that must never be fetched.
      busybox = sql("SELECT id FROM derivation WHERE name = '${pkgs.busybox.name}';")
      busywrap = sql("SELECT id FROM derivation WHERE name = 'busywrap';")
      assert busybox, "the evaluation walked no derivation named ${pkgs.busybox.name}"
      assert busywrap, "the evaluation walked no derivation named busywrap"

      assert anchor_of(busywrap).startswith("3 0"), (
          f"busywrap must be built here, not relayed: {anchor_of(busywrap)}"
      )
      # `7`, not `3`: a relay ran no build, so the worker reports it substituted
      # and the anchor settles `Substituted`. Built here reads `3`, which is what
      # busywrap is asserted on one line above - the pair is the whole point.
      assert anchor_of(busybox) == "7 1 1", (
          f"busybox must be relayed exactly once off the upstream: {anchor_of(busybox)}"
      )
      # The whole of decision 3: the worker fetches an output and nothing below
      # it, so the server demands the producers of what the NAR references and
      # each is relayed on its own. That is what makes the output whole and its
      # dependents buildable entirely out of our cache.
      assert output_missing(busybox) == "0", (
          f"busybox's relayed output is missing closure members: {output_missing(busybox)}"
      )
      # The point of #666: a relay needs none of its inputs, so none of them may be
      # built. busybox's source FODs reach the network, which the VM does not have,
      # so before the fix they were dispatched, failed permanently, and cascaded onto
      # the anchor the moment a retire made it non-terminal again.
      relayed_inputs_built = sql(
          "SELECT count(*) FROM build_attempt a "
          "JOIN derivation_build db ON db.id = a.derivation_build "
          "JOIN derivation d ON d.id = db.derivation "
          "WHERE d.name LIKE 'unzip60%' OR d.name LIKE 'CVE-2019-13232%' "
          "   OR d.name LIKE 'patchutils%';"
      )
      assert relayed_inputs_built == "0", (
          f"a relayed anchor's inputs were built anyway: {relayed_inputs_built} attempts"
      )
      # The source is the input the relay exists to avoid BUILDING. Dispatched is
      # not the invariant: `separateDebugInfo` puts the source inside busybox's
      # runtime closure, so the walk demands it over a `Both` edge and it is
      # relayed off the same upstream - which is the only way busybox ever reads
      # whole. Never BUILT is the invariant, and a relay is not a build: it
      # settles `Substituted` off bytes we already have a URL for, while a build
      # of this FOD would reach a network the VM does not have.
      sources, built, statuses = sql(
          f"SELECT count(*)::text || ' ' || count(*) FILTER ("
          f"  WHERE db.status = 3 OR (NOT (db.substitutable OR db.substituted) AND EXISTS ("
          f"    SELECT 1 FROM build_attempt a WHERE a.derivation_build = db.id)))::text "
          f"|| ' ' || coalesce(string_agg(DISTINCT db.status::text, ','), '-') "
          f"FROM derivation_build db JOIN derivation d ON d.id = db.derivation "
          f"JOIN derivation_dependency e ON e.dependency = d.id AND e.derivation = '{busybox}' "
          f"WHERE d.name LIKE '%.tar%';"
      ).split()
      assert int(sources) >= 1 and built == "0", (
          f"busybox's source may be relayed, never built: "
          f"{sources} sources, {built} built, status {statuses}"
      )
      assert runtime_drift() == 0, "wholeness disagrees with its recompute after the relay"
      assert anchor_drift() == 0, "anchor counters disagree with their recompute after the relay"
      assert demand_drift() == 0, "demand disagrees with its recompute after the relay"

      # A re-evaluation demands the same anchor, which is already whole, so the
      # gate holds and nothing is relayed again. 14k anchors on production were
      # re-relayed exactly here.
      server.succeed(f"{GIT} -C /var/lib/git/test commit --allow-empty -m 'busywrap again'")
      server.succeed("chown git:git -R /var/lib/git/test")
      eval5_id = wait_for_new_eval({eval_id, eval2_id, eval3_id, eval4_id})
      assert anchor_of(busybox) == "7 1 1", (
          f"the second evaluation re-relayed a whole anchor: {anchor_of(busybox)}"
      )

      # Retire busybox's NAR. Its anchor loses fetchability and is reset to a
      # fresh build intent, but busywrap is terminal and no entry point names
      # busybox, so nothing demands it: it stays `Created` and is never
      # dispatched. This is the assertion the old undemanded-relay behaviour
      # cannot pass.
      bb_hash = output_hash(busybox)
      assert bb_hash, "busybox has no cached output row to retire"
      server.succeed(f"rm -f {nar_object(bb_hash)}")
      server.succeed(
          f"{CURL} -sf -X POST -H 'Authorization: Bearer {token}' "
          f"{API}/admin/maintenance/deep-gc"
      )
      poll(f"SELECT count(*) FROM cached_path WHERE hash = '{bb_hash}';", "0",
           "the zombie purge kept busybox's row after its NAR was deleted")
      poll(anchor_column(busybox, "db.status::text"), "0",
           "the retire left busybox terminal-success with nothing to serve")

      # Three dispatch ticks and a maintenance pass: the consistency sweep is the
      # other claimant for a `Created` anchor and its promote embeds the same
      # gate, so if demand were not part of that gate it would queue busybox here.
      server.sleep(45)
      assert anchor_of(busybox) == "0 1 1", (
          f"an undemanded relay was dispatched again: {anchor_of(busybox)}"
      )

      # Retire busywrap's own output. Its producer is reset, which makes it a
      # builder again, which demands busybox: the relay runs a second time and
      # both anchors come back.
      bw_hash = output_hash(busywrap)
      assert bw_hash, "busywrap has no cached output row to retire"
      server.succeed(f"rm -f {nar_object(bw_hash)}")
      server.succeed(
          f"{CURL} -sf -X POST -H 'Authorization: Bearer {token}' "
          f"{API}/admin/maintenance/deep-gc"
      )
      poll(f"SELECT count(*) FROM cached_path WHERE hash = '{bw_hash}';", "0",
           "the zombie purge kept busywrap's row after its NAR was deleted")
      poll(relay_attempts(busybox), "2",
           "a demanded relay was not re-dispatched after its NAR was retired",
           timeout=600)
      poll(anchor_column(busywrap, "db.status::text"), "3",
           "busywrap was not rebuilt once its input was relayed again", timeout=600)
      assert output_missing(busybox) == "0", (
          f"the second relay left closure members behind: {output_missing(busybox)}"
      )
      assert runtime_drift() == 0, "wholeness disagrees with its recompute after the re-relay"
      assert anchor_drift() == 0, "anchor counters disagree with their recompute after the re-relay"
      assert demand_drift() == 0, "demand disagrees with its recompute after the re-relay"

      # ── Phase 10h: a pruned interior outlives the evaluation that walked it ─
      # A batch names what it walked plus the direct inputs of that, and a walk
      # prunes on `walked` alone, so the interior of a subtree another evaluation
      # walked first is named by that evaluation and by nobody else. Delete it
      # while this one still builds against the subtree and every gate in the
      # interior shuts at once: promotion, the dispatch select, the dispatcher's
      # driving evaluation and eval-done all read `build_job` (#663). The fix
      # hands the names over: a live evaluation adopts the pending anchors it
      # reaches through its own builders, from the GC's own pass, the graph-stuck
      # heal and the consistency sweep. This phase makes the state by hand - the
      # interior's names are dropped, its outputs are retired for real so the
      # chain is pending, task2's current evaluation is put back into Building over
      # it - and asserts the outcome end to end: adopted, queued, attributed to
      # that evaluation, rebuilt, and it completes with everything whole.
      banner("Phase 10h: a pruned interior is adopted, attributed and rebuilt (#663)")

      hello_drv = sql(f"SELECT id FROM derivation WHERE hash = '{drv_hash}';")
      assert hello_drv, "hello's derivation row is gone"

      def built_input_of(derivation, needs_inputs):
          """The least-shared direct input that is a walked, non-substitutable,
          terminal-success builder with whole outputs and a whole .drv: ready to
          be queued the moment it is reset."""
          inputs = (
              "AND EXISTS (SELECT 1 FROM derivation_dependency i WHERE i.derivation = e.dependency) "
              if needs_inputs else ""
          )
          return sql(
              f"SELECT e.dependency FROM derivation_dependency e "
              f"JOIN derivation_build db ON db.derivation = e.dependency "
              f"JOIN derivation d ON d.id = e.dependency "
              f"JOIN cached_path drv ON drv.hash = d.hash "
              f"WHERE e.derivation = '{derivation}' AND d.walked AND NOT db.substitutable "
              f"  AND db.status IN (3, 7) AND db.fetchable AND db.unready_deps = 0 "
              f"  AND drv.file_hash IS NOT NULL {inputs}"
              f"ORDER BY (SELECT count(*) FROM derivation_dependency r WHERE r.dependency = e.dependency), "
              f"         d.name LIMIT 1;"
          )

      d1 = built_input_of(hello_drv, needs_inputs=True)
      d2 = built_input_of(d1, needs_inputs=False) if d1 else ""
      assert d1 and d2, f"hello has no two-deep chain of rebuildable inputs (d1={d1!r}, d2={d2!r})"
      chain = f"'{hello_drv}', '{d1}', '{d2}'"
      print("chain under test: " + sql(
          f"SELECT string_agg(d.name, ' > ' ORDER BY d.id = '{hello_drv}' DESC, d.id = '{d1}' DESC) "
          f"FROM derivation d WHERE d.id IN ({chain});"
      ))
      outputs = sql(f"SELECT o.hash FROM derivation_output o WHERE o.derivation IN ({chain});").split()
      assert len(outputs) >= 3, f"the chain has only {len(outputs)} outputs"

      # What keep_evaluations leaves behind for a pruned interior once its walker
      # is gone: no evaluation names it. hello keeps its names.
      sql(f"DELETE FROM build_job WHERE derivation IN ('{d1}', '{d2}');")
      assert sql(f"SELECT count(*) FROM build_job WHERE derivation IN ('{d1}', '{d2}');") == "0"

      # Retire the chain's outputs for real, so the producers are reset and the
      # counters move by the retire's own ripple rather than by hand.
      for h in outputs:
          server.succeed(f"rm -f {nar_object(h)}")
      server.succeed(
          f"{CURL} -sf -X POST -H 'Authorization: Bearer {token}' "
          f"{API}/admin/maintenance/deep-gc"
      )
      for h in outputs:
          poll(f"SELECT count(*) FROM cached_path WHERE hash = '{h}';", "0",
               f"the zombie purge kept {h}'s row after its NAR was deleted")
      poll(f"SELECT count(*) FROM derivation_build WHERE derivation IN ({chain}) AND status = 0;", "3",
           "the retire did not reset every producer of the chain")
      assert runtime_drift() == 0, "wholeness disagrees with its recompute after the retire"
      assert anchor_drift() == 0, "anchor counters disagree with their recompute after the retire"

      # The dead zone, stated: nothing queues an unnamed anchor, however ready.
      server.sleep(20)
      assert sql(f"SELECT status::text FROM derivation_build WHERE derivation = '{d2}';") == "0", (
          "an interior with no name was queued before anything adopted it"
      )

      # task2's CURRENT evaluation is the one still building against the subtree.
      # Not `eval2_id`: every push in the phases above re-evaluates both tasks and
      # `keep_evaluations` is 1, so task2's first evaluation and its names are long
      # deleted by here - which is the very state this phase is about. Nothing above
      # waits on task2's side of a push, and phase 10g made two commits its own 10 s
      # poll picks up, so its newest evaluation is still walking as often as not:
      # wait for the batch that names the builder before reading the id.
      newest_task2 = (
          "SELECT e.id FROM evaluation e JOIN task t ON t.id = e.task "
          "WHERE t.name = 'task2' ORDER BY e.created_at DESC LIMIT 1"
      )
      poll(f"SELECT count(*) FROM build_job bj WHERE bj.derivation = '{hello_drv}' "
           f"AND bj.evaluation = ({newest_task2});",
           "1", "task2's newest evaluation never named the builder the adoption walks out of",
           timeout=420)
      task2_eval = sql(f"{newest_task2};")
      assert task2_eval, "task2 has no evaluation left to build against the subtree"
      sql(f"UPDATE evaluation SET status = 3, "
          f"building_started_at = (now() AT TIME ZONE 'UTC'), updated_at = (now() AT TIME ZONE 'UTC') "
          f"WHERE id = '{task2_eval}';")
      assert sql(
          f"SELECT count(*) FROM build_job bj WHERE bj.evaluation = '{task2_eval}' "
          f"AND bj.derivation = '{hello_drv}';"
      ) == "1", "task2's evaluation does not name the builder the adoption walks out of"

      poll(f"SELECT count(*) FROM build_job WHERE evaluation = '{task2_eval}' AND derivation IN ('{d1}', '{d2}');",
           "2", "task2's evaluation did not adopt the interior it never walked", timeout=420)
      status2 = ""
      for _ in range(60):
          status2 = server.succeed(
              f'{CURL} -sf -H "Authorization: Bearer {token}" '
              f'{API}/evals/{task2_eval} | {JQ} -rj ".message.status"'
          ).strip()
          if status2 == "Completed":
              break
          if status2 in ("Failed", "Aborted"):
              j = server.succeed("journalctl -u gradient-server --no-pager --since='-600s' -n 300")
              raise Exception(f"task2's evaluation ended {status2} over the adopted chain:\n{j[-3000:]}")
          server.sleep(10)
      else:
          raise Exception(f"task2's evaluation did not complete over the adopted chain in 600 s (still {status2!r})")

      assert sql(
          f"SELECT count(*) FROM derivation_build WHERE derivation IN ({chain}) AND status IN (3, 7) AND fetchable;"
      ) == "3", "the chain is not terminal-success and fetchable again"
      for h in outputs:
          assert sql(
              f"SELECT count(*) FROM cached_path WHERE hash = '{h}' AND file_hash IS NOT NULL;"
          ) == "1", f"{h} did not come back"
      assert sql(
          f"SELECT count(*) FROM derivation_build WHERE derivation IN ({chain}) "
          f"AND missing_runtime_deps = 0;"
      ) == "3", "the chain did not come back whole"
      attributed = int(sql(
          f"SELECT count(DISTINCT bj.derivation) FROM build_attempt a JOIN build_job bj ON bj.id = a.build_job "
          f"WHERE bj.evaluation = '{task2_eval}' AND bj.derivation IN ('{d1}', '{d2}');"
      ))
      assert attributed == 2, f"the interior's rebuilds were attributed to {attributed} of the 2 adopted names"
      assert runtime_drift() == 0, "wholeness disagrees with its recompute after the adopted rebuild"
      assert anchor_drift() == 0, "anchor counters disagree with their recompute after the adopted rebuild"
      print(server.succeed("journalctl -u gradient-server --no-pager | grep -i 'adopt' | tail -n 5"))

      # ── Phase 10i: retention follows the live closure (#594) ──────────────
      # One keep-set decides what stays in the cache: the NAR reference closure
      # of the outputs and `.drv` files of every derivation a retained
      # evaluation reaches. Phase 9's probe is a store path no derivation
      # produced and no evaluation names, so it has been outside that set since
      # the moment it was uploaded and only its fetch recency keeps it; hello is
      # inside it until nothing names hello any more.
      banner("Phase 10i: live-closure retention (#594)")

      probe_object = nar_object(f"{upload_hash}-probe")
      assert sql(f"SELECT count(*) FROM cached_path WHERE hash = '{upload_hash}';") == "1", \
          "phase 9's probe left the cache before this phase could evict it"
      server.succeed(f"test -e {probe_object}")

      def age(hashes, hours):
          """Backdate the commit and the last fetch of `hashes` by `hours`."""
          listed = "', '".join(hashes)
          sql(f"UPDATE cached_path SET created_at = created_at - interval '{hours} hours' "
              f"WHERE hash IN ('{listed}');")
          sql(f"UPDATE cached_path_signature SET last_fetched_at = last_fetched_at - interval '{hours} hours' "
              f"WHERE cached_path IN (SELECT id FROM cached_path WHERE hash IN ('{listed}'));")

      # 26 hours, not 2: the bound is the fetch TTL floored at the upload grace
      # (`cacheTtlHours = 1`, `narUploadGraceHours = 24`), so a closure member is
      # never reclaimed between its own commit and its referrer's.
      age([upload_hash], 26)
      poll(f"SELECT count(*) FROM cached_path WHERE hash = '{upload_hash}';", "0",
           "the eviction kept a path no retained evaluation reaches", timeout=240)
      server.fail(f"test -e {probe_object}")
      assert sql(
          f"SELECT count(*) FROM cached_path WHERE hash IN ('{store_hash}', '{dep_hash}');"
      ) == "2", "the eviction took a path the live closure still reaches"
      assert runtime_drift() == 0, "wholeness disagrees with its recompute after the eviction"

      # `build_job` and `entry_point` are what seed the reachable walk, so hello
      # leaves the live set when its names go; the pollers would write them back,
      # so the triggers go first. Its derivation row then ages past the orphan
      # grace, and the eviction pass owns the NARs the derivation GC used to.
      sql("DELETE FROM task_trigger;")
      hello_drv = sql(f"SELECT id FROM derivation WHERE hash = '{drv_hash}';")
      assert hello_drv, "hello's derivation row is gone before this phase deleted anything"
      hello_paths = [h for h in (store_hash, drv_hash)
                     if sql(f"SELECT count(*) FROM cached_path WHERE hash = '{h}';") == "1"]
      assert store_hash in hello_paths, "hello's output is not cached; the eviction would prove nothing"

      sql(f"DELETE FROM build_job WHERE derivation = '{hello_drv}';")
      sql(f"DELETE FROM entry_point WHERE derivation = '{hello_drv}';")
      sql(f"UPDATE derivation SET created_at = created_at - interval '48 hours' WHERE id = '{hello_drv}';")
      age(hello_paths, 26)

      poll(f"SELECT count(*) FROM derivation WHERE id = '{hello_drv}';", "0",
           "the orphan GC kept a derivation nothing reaches", timeout=240)
      listed = "', '".join(hello_paths)
      poll(f"SELECT count(*) FROM cached_path WHERE hash IN ('{listed}');", "0",
           "hello's paths outlived the only derivation that reached them", timeout=240)
      for h in hello_paths:
          server.fail(f"test -e {nar_object(h + '-hello')}")
      assert sql(f"SELECT count(*) FROM cached_path WHERE hash = '{dep_hash}';") == "1", \
          "a path its own name still reaches left the cache with hello"
      assert runtime_drift() == 0, "wholeness disagrees with its recompute after the GC and the eviction"

      # ── Phase 10i: a naming racing a transition settles anyway ────────────
      # The counter triggers take no lock: a naming held FOR SHARE until the
      # ingest flush commits deadlocks that flush against a ripple. So a naming
      # and a transition in flight together miss each other, and the evaluation
      # counts an anchor that already finished. The exact reads are the arbiter:
      # the tick's anchor read finds nothing blocking, recounts, and settles. This
      # parks a finished evaluation back in `Building` behind a held anchor, runs
      # that race on a second anchor, releases the first, and waits for the server
      # to settle it with counters that match their recount.
      banner("Phase 10i: a naming racing a transition still settles (#640)")
      CR_DRV = "1950ecc8-8530-4e1c-bc1c-de4bd759fe57"
      CR_HOLD_DRV = "5f923367-19cc-4823-9b73-5c4b9fe4d0ca"
      cr_eval = sql(
          "SELECT e.id FROM evaluation e WHERE e.status = 5 AND NOT EXISTS ("
          "SELECT 1 FROM build_job bj JOIN derivation_build db ON db.id = bj.derivation_build "
          "WHERE bj.evaluation = e.id AND db.status NOT IN (3, 7)) "
          "ORDER BY e.created_at LIMIT 1;"
      )
      assert cr_eval, "no cleanly completed evaluation to park for the race"

      def counterrace_anchor(drv, name, status):
          sql(
              f"INSERT INTO derivation (id, created_at, architecture, hash, name, "
              f"prefer_local_build, allow_substitutes, is_fixed_output, walked) VALUES "
              f"('{drv}', now() AT TIME ZONE 'UTC', 'x86_64-linux', '{name.ljust(32, '0')}', "
              f"'{name}', false, true, false, false);\n"
              f"INSERT INTO derivation_build (id, derivation, status, substitutable, substituted, "
              f"fetchable, unready_deps, attempt, demanded, created_at, updated_at) VALUES "
              f"(uuidv7(), '{drv}', {status}, false, false, false, 0, 0, true, "
              f"now() AT TIME ZONE 'UTC', now() AT TIME ZONE 'UTC');"
          )
          return sql(f"SELECT id FROM derivation_build WHERE derivation = '{drv}';")

      cr_anchor = counterrace_anchor(CR_DRV, "counterrace", 0)
      cr_hold = counterrace_anchor(CR_HOLD_DRV, "counterhold", 2)

      server.succeed(f"cat > /tmp/counterrace.sh <<'COUNTERRACE'\n{COUNTER_RACE_SH}\nCOUNTERRACE")
      print(server.succeed(
          f"CR_EVAL={cr_eval} CR_ANCHOR={cr_anchor} CR_HOLD={cr_hold} sh /tmp/counterrace.sh"
      ))

      poll(f"SELECT status FROM evaluation WHERE id = '{cr_eval}';", "5",
           "an evaluation whose counters kept an anchor the race hid never settled", timeout=120)
      agree = sql(
          "SELECT (e.named_anchors + coalesce((SELECT sum(d.named) FROM evaluation_anchor_delta d WHERE d.evaluation = e.id), 0), "
          "e.active_anchors + coalesce((SELECT sum(d.active) FROM evaluation_anchor_delta d WHERE d.evaluation = e.id), 0)) "
          "= (SELECT (count(*), coalesce(sum(x.active), 0)) FROM build_job bj "
          "JOIN derivation_build db ON db.id = bj.derivation_build "
          "CROSS JOIN LATERAL evaluation_anchor_counts(db.status, db.demanded) x "
          f"WHERE bj.evaluation = e.id) FROM evaluation e WHERE e.id = '{cr_eval}';"
      )

      sql(
          f"DELETE FROM build_job WHERE derivation_build IN ('{cr_anchor}', '{cr_hold}');\n"
          f"DELETE FROM derivation_build WHERE id IN ('{cr_anchor}', '{cr_hold}');\n"
          f"DELETE FROM derivation WHERE id IN ('{CR_DRV}', '{CR_HOLD_DRV}');"
      )
      assert agree == "t", "the evaluation settled but its counters still disagree with their recount"

      # ── Phase 10j: two instances claiming one job, one wins ──────────────
      # Scores come from the workers, so each instance's tracker proposes a
      # winner on its own; the claim decides it in Postgres. Two sessions stand
      # in for two instances claiming the same anchor: the unique index on the
      # open job key makes the second wait for the first and insert nothing, so
      # the job is out exactly once. The anchor has no build_job, so the running
      # server never dispatches it.
      banner("Phase 10j: two instances claiming one job, one wins (#641)")
      CL_DRV = "8ef77385-09cc-40f8-833e-732d330a2685"
      cl_owner = sql("SELECT evaluation_id || ' ' || project FROM dispatched_job LIMIT 1;")
      assert cl_owner, "no dispatched job to borrow an evaluation and project from"
      cl_eval, cl_project = cl_owner.split()
      sql(
          f"INSERT INTO derivation (id, created_at, architecture, hash, name, "
          f"prefer_local_build, allow_substitutes, is_fixed_output, walked) VALUES "
          f"('{CL_DRV}', now() AT TIME ZONE 'UTC', 'x86_64-linux', '{'claimrace'.ljust(32, '0')}', "
          f"'claimrace', false, true, false, true);\n"
          f"INSERT INTO derivation_build (id, derivation, status, substitutable, substituted, "
          f"fetchable, unready_deps, attempt, demanded, created_at, updated_at) VALUES "
          f"(uuidv7(), '{CL_DRV}', 1, false, false, false, 0, 0, true, "
          f"now() AT TIME ZONE 'UTC', now() AT TIME ZONE 'UTC');"
      )
      cl_anchor = sql(f"SELECT id FROM derivation_build WHERE derivation = '{CL_DRV}';")

      server.succeed(f"cat > /tmp/claimrace.sh <<'CLAIMRACE'\n{CLAIM_RACE_SH}\nCLAIMRACE")
      out = server.succeed(
          f"CL_ANCHOR={cl_anchor} CL_EVAL={cl_eval} CL_PROJECT={cl_project} sh /tmp/claimrace.sh"
      )
      print(out)
      open_rows = sql(
          f"SELECT count(*) FROM dispatched_job WHERE job_id = 'build:{cl_anchor}' AND finished_at IS NULL;"
      )

      sql(
          f"DELETE FROM dispatched_job WHERE job_id = 'build:{cl_anchor}';\n"
          f"DELETE FROM derivation_build WHERE id = '{cl_anchor}';\n"
          f"DELETE FROM derivation WHERE id = '{CL_DRV}';"
      )
      assert "claimrace_a INSERT 0 1" in out, f"the first claim did not win: {out}"
      assert "claimrace_b INSERT 0 0" in out, f"the second claim inserted a duplicate: {out}"
      assert open_rows == "1", f"{open_rows} open rows for one job key"

      # ── Phase 10k: a seed and a flip see each other (#643) ────────────────
      # A seed counts its dependencies under shared advisory keys, a flip holds its
      # anchor's key exclusively, so whichever runs second reads the other's rows.
      banner("Phase 10k: a seed and a flip see each other through the anchor keys (#643)")
      server.succeed(f"cat > /tmp/lockguard.sh <<'LOCKGUARD'\n{LOCK_GUARD_SH}\nLOCKGUARD")
      out = server.succeed(
          "LG_PSQL='su postgres -c \"psql -X -At -d gradient\"' "
          "LG_GATE=${pkgs.gradient.gate}/bin/gradient-sql-gate bash /tmp/lockguard.sh 2>&1"
      )
      print(out)
      for arm in ("flip-first", "seed-first", "unwhole", "retire-reads", "shared"):
          assert f"lockguard {arm}: recount wrote 0" in out, f"arm {arm} drifted:\n{out}"
      assert "lockguard unguarded: recount wrote 1" in out, (
          f"the unguarded arm did not drift, so the keys are not what the others prove:\n{out}"
      )

      # ── Phase 11: the supervision tree is healthy and shutdown drains ─────
      banner("Phase 11: every supervised loop is running; SIGTERM drains")
      health = json.loads(api_get(token, "board/health"))["message"]
      names = sorted(l["name"] for l in health["supervised"])
      print(names)
      for want in ["graph", "effects", "build-dispatch", "eval-dispatch", "trigger-dispatch",
                   "cache-maintenance", "sign-sweep", "debug-index",
                   "eval-cache-sweep", "retention", "rollup", "outbound-connect",
                   "nar-uploader"]:
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

      # ── Phase 14: an outbox row written before a restart is delivered after it ─
      # The point of the outbox: an effect owed to the outside world is a row, so
      # it survives the process that owed it. The row is inserted while the server
      # is down, which no in-process spawn could have carried across.
      banner("Phase 14: the outbox survives a restart (#597)")
      action_id = sql("SELECT id FROM task_action WHERE name = 'restart-probe';")
      assert len(action_id) == 36, f"the state-declared probe action is missing: {action_id}"

      server.succeed("cat > /root/hook.py <<'PY'\n"
                     "from http.server import BaseHTTPRequestHandler, HTTPServer\n"
                     "class H(BaseHTTPRequestHandler):\n"
                     "    def do_POST(self):\n"
                     "        n = int(self.headers.get('Content-Length', 0))\n"
                     "        open('/root/hook.log', 'ab').write(self.rfile.read(n) + b'\\n')\n"
                     "        self.send_response(200); self.end_headers()\n"
                     "HTTPServer(('127.0.0.1', 8099), H).serve_forever()\n"
                     "PY")
      server.succeed("systemd-run --unit hook-probe ${pkgs.python3}/bin/python3 /root/hook.py")

      server.succeed("systemctl stop gradient-server.service")
      sql(
          "INSERT INTO outbox (id, kind, key, payload, created_at, next_attempt_at) VALUES ("
          "gen_random_uuid(), 3, 'restart-probe', "
          f"""'{{"action": "{action_id}", "event": "evaluation.approval_granted", "payload": {{"probe": "restart"}}}}'::jsonb, """
          "now() AT TIME ZONE 'UTC', now() AT TIME ZONE 'UTC');"
      )
      server.succeed("systemctl start gradient-server.service")
      server.wait_for_open_port(3000)

      server.wait_until_succeeds("grep -q restart /root/hook.log", timeout=120)
      assert sql("SELECT delivered_at IS NOT NULL FROM outbox WHERE key = 'restart-probe';") == "t", \
          "the row must settle, not be redelivered on every tick"

      # ── Phase 13: every registered statement plans sanely (#651) ──────────
      # The gate amplifies this database to production scale, so it runs last and
      # the fleet is stopped first: nothing else should ever see those rows, and a
      # worker left running only reconnects at a server that is gone. It explains
      # each statement in a transaction it rolls back, which is what makes a
      # registered INSERT, UPDATE, DELETE or FOR UPDATE safe to ANALYZE.
      banner("Phase 13: the SQL plan gate")
      # A statement over an empty table is unmeasured, and nothing earlier stars.
      for star in ("projects/project", "tasks/project/task", "caches/main"):
          server.succeed(
              f"{CURL} -sf -X PUT -H 'Authorization: Bearer {token}' "
              f"{API}/user/stars/{star}"
          )
      builder.succeed("systemctl stop gradient-worker.service")
      builder2.succeed("systemctl stop gradient-worker.service")
      server.succeed("systemctl stop gradient-server.service")

      print(server.succeed(
          "${pkgs.gradient.gate}/bin/gradient-sql-gate "
          "--database-url postgresql://postgres@127.0.0.1/gradient "
          "--max-unmeasured 40 2>&1"
      ))

      banner("E2E test PASSED")
      '';
  });
}
