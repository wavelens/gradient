/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ lib, config, ... }: with lib; let
  upstreamType = types.submodule {
    options = {
      type = mkOption {
        type = types.enum [ "internal" "external" ];
        description = "Type of upstream: internal (another Gradient cache) or external (Nix binary cache URL)";
      };

      cache_name = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = "Name of the internal Gradient cache to use as upstream (required for internal type)";
      };

      display_name = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = "Display name for the upstream (optional for internal, required for external)";
      };

      mode = mkOption {
        type = types.enum [ "ReadWrite" "ReadOnly" "WriteOnly" ];
        default = "ReadWrite";
        description = "Access mode for internal upstreams (ignored for external, which is always ReadOnly)";
      };

      url = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = "URL of the external Nix binary cache (required for external type)";
      };

      public_key = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = "Public key of the external Nix binary cache (required for external type)";
      };
    };
  };

  userType = types.submodule ({ config, name, ... }: {
    options = {
      username = mkOption {
        type = types.str;
        default = name;
        defaultText = "<attrset key>";
        description = "Unique username for the user";
      };

      name = mkOption {
        type = types.str;
        default = config.username;
        defaultText = "config.username";
        description = "Full name of the user";
      };

      email = mkOption {
        type = types.str;
        description = "Email address of the user";
      };

      password_file = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = ''
          Path to file containing the hashed password. Leave null for
          OIDC-only users - the provisioned account will be created without
          a local password, so the OIDC login flow can claim it by email.
        '';
      };

      email_verified = mkOption {
        type = types.bool;
        default = true;
        description = "Whether the user's email has been verified";
      };

      superuser = mkOption {
        type = types.bool;
        default = false;
        description = "Whether the user has superuser privileges";
      };
    };
  });

  organizationType = types.submodule ({ config, name, ... }: {
    options = {
      name = mkOption {
        type = types.str;
        default = name;
        defaultText = "<attrset key>";
        description = "Unique name for the organization";
      };

      display_name = mkOption {
        type = types.str;
        default = config.name;
        defaultText = "config.name";
        description = "Display name for the organization";
      };

      id = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = ''
          Explicit organization UUID. When set, a freshly created
          organization is given this id instead of a server-generated one,
          so a worker's `peersFile` can reference it (`<id>:<token>`) in a
          fully declarative deployment without first looking up the
          auto-generated id. Applied on create only; the id is immutable, so
          a value conflicting with an existing organization is rejected.

          Generate one with `uuidgen`.
        '';
      };

      description = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = "Description of the organization";
      };

      private_key_file = mkOption {
        type = types.str;
        description = "Path to SSH private key file for Git access";
      };

      public = mkOption {
        type = types.bool;
        default = false;
        description = "Whether the organization is public (visible to all users)";
      };

      hide_build_requests = mkOption {
        type = types.bool;
        default = false;
        description = ''
          When `true`, the auto-managed `build-request` project for this
          organization is hidden from project listings in the web UI. The
          project still exists and continues to receive evaluations from the
          `gradient build` CLI; this is a UI-only opt-out.
        '';
      };

      created_by = mkOption {
        type = types.str;
        description = "Username of the user who created this organization";
      };

      members = mkOption {
        type = types.listOf orgMemberType;
        default = [];
        description = ''
          Users with role assignments on this organization. When empty
          (the default), legacy behavior applies: `created_by` is added
          as Admin and no other membership reconciliation happens.

          When non-empty, this list is the source of truth - existing
          memberships not in the list are revoked on next state apply,
          and the implicit `created_by`-as-Admin assignment is skipped
          (list yourself explicitly if you want that role).

          Members referencing users that do not yet exist are skipped
          silently at provision time and applied automatically when the
          user later registers (`POST /user`) or signs in via OIDC for
          the first time.
        '';
        example = literalExpression ''
          [
            { user = "alice"; role = "Admin"; }
            { user = "bob";   role = "Write"; }
            { user = "carol"; role = "releaser"; }
          ]
        '';
      };
    };
  });

  flakeInputOverrideType = types.submodule ({ ... }: {
    options = {
      url = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = ''
          Flake reference to use as the override for this input. When
          null and `keep_url` is true, the input is force-updated using
          the URL declared in the project's flake.nix.
        '';
      };
      keep_url = mkOption {
        type = types.bool;
        default = false;
        description = ''
          When true, force an update of this input using its original
          flake-declared URL (without changing the URL). Mutually
          exclusive with `url` - exactly one of the two must be set.
        '';
      };
    };
  });

  projectType = types.submodule ({ config, name, ... }: {
    options = {
      name = mkOption {
        type = types.str;
        default = name;
        defaultText = "<attrset key>";
        description = "Unique name for the project";
      };

      organization = mkOption {
        type = types.str;
        description = "Name of the organization this project belongs to";
      };

      display_name = mkOption {
        type = types.str;
        default = config.name;
        defaultText = "config.name";
        description = "Display name for the project";
      };

      description = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = "Description of the project";
      };

      repository = mkOption {
        type = types.str;
        description = "Git repository URL for the project";
      };

      wildcard = mkOption {
        type = types.str;
        default = "packages.x86_64-linux.*";
        description = "Branch or pattern for evaluations";
      };

      active = mkOption {
        type = types.bool;
        default = true;
        description = "Whether the project is active";
      };

      keep_evaluations = mkOption {
        type = types.ints.positive;
        default = 1;
        description = ''
          Number of finished evaluations to retain per project for metrics
          and history. The most recent finished evaluations are kept regardless
          of outcome (completed, failed, or aborted), and GC is skipped while the
          project has an in-progress evaluation. Older evaluations beyond this
          count are garbage-collected. Must be at least 1; capped at runtime by
          the global `services.gradient.settings.keepEvaluations`.
        '';
      };

      sign_cache = mkOption {
        type = types.bool;
        default = true;
        description = ''
          When `false`, build outputs from this project are pushed to the
          cache but their narinfo signatures are left empty, so external
          Nix clients won't trust them - keeping the project's outputs
          private even when the cache itself is public. A path co-produced
          by another `sign_cache = true` project is still signed.
        '';
      };

      concurrency = mkOption {
        type = types.enum [ "hard_abort" "soft_abort" "skip" "all" ];
        default = "soft_abort";
        description = ''
          Project-level policy for handling new trigger events while an
          evaluation is in flight.

          - `hard_abort` cancels the running evaluation (and its in-flight
            builds) and starts a fresh one.
          - `soft_abort` marks the running evaluation Aborted so the new one
            becomes canonical, but lets in-flight builds finish; their cached
            outputs flow into the new evaluation.
          - `skip` discards the new trigger event.
          - `all` runs a new evaluation alongside the in-flight one
            (multi-eval per project).
        '';
      };

      triggers = mkOption {
        type = types.nullOr (types.listOf triggerType);
        default = null;
        description = ''
          List of evaluation triggers for the project. Each trigger declares
          *how* and *when* an evaluation runs (polling, forge push, forge PR,
          cron schedule). When `null`, existing trigger rows are left
          untouched (back-compat for state files predating this option). When
          set to `[]`, provisioning errors out - every project must have at
          least one trigger.

          A new project always receives a default polling trigger
          (interval 300s) automatically; declaring `triggers` here replaces
          that default with the listed set.
        '';
        example = literalExpression ''
          [
            {
              type = "polling";
              config = { interval_secs = 60; };
            }
            {
              type = "reporter_push";
              integration = "acme-prod-inbound";
              config = { branches = [ "main" "release/*" ]; };
            }
            {
              type = "time";
              config = { cron = "0 0 2 * * *"; };
            }
          ]
        '';
      };

      flake_input_overrides = mkOption {
        type = types.attrsOf flakeInputOverrideType;
        default = {};
        description = ''
          Per-input overrides applied during flake fetch. The attribute
          key is the flake input name. Empty (`{}`) means no overrides
          - flake.lock is used as-is.
        '';
        example = literalExpression ''
          {
            nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
            flake-utils.keep_url = true;
          }
        '';
      };

      actions = mkOption {
        type = types.listOf actionType;
        default = [];
        description = ''
          Project actions (email notifications, outbound web requests, forge
          status reports, pull-request automation). Re-applying state with
          fewer actions removes the missing ones (matched by `name` within
          the project).

          Token files for `send_web_request` actions must live at the systemd
          credential path
          `''${GRADIENT_CREDENTIALS_DIR}/gradient_action_''${name}_token`.
        '';
        example = literalExpression ''
          [
            {
              name = "notify-ops";
              type = "send_mail";
              events = [ "build.failed" ];
              config = {
                recipients = [ "ops@example.com" ];
                subject_template = null;
              };
            }
            {
              name = "notify-hooks";
              type = "send_web_request";
              events = [ "build.completed" "build.failed" ];
              config = {
                url = "https://hooks.example.com/gradient";
                token_file = "/etc/gradient/secrets/notify-hooks-token";
              };
            }
            {
              name = "report-status";
              type = "forge_status_report";
              config = { integration = "gitea-prod"; };
            }
            {
              name = "flake-lock-pr";
              type = "open_pr";
              config = {
                integration = "gitea-prod";
                generator = "flake_lock";
                granularity = "per_input";
                verify_gate = "build";
                branch_pattern = "gradient/flake-lock-update/{input}";
                title_template = "flake.lock: update {input}";
                body_template = "Automated flake input update opened by Gradient.";
                update_existing = true;
              };
            }
          ]
        '';
      };

      created_by = mkOption {
        type = types.str;
        description = "Username of the user who created this project";
      };
    };
  });

  integrationType = types.submodule ({ config, name, ... }: {
    options = {
      name = mkOption {
        type = types.str;
        default = name;
        defaultText = "<attrset key>";
        description = "Unique name for the integration within (organization, kind)";
      };

      display_name = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = "Human-readable display name for the integration. Defaults to `name` when null.";
      };

      organization = mkOption {
        type = types.str;
        description = "Name of the organization this integration belongs to";
      };

      kind = mkOption {
        type = types.enum [ "inbound" "outbound" ];
        description = ''
          `inbound` - the forge calls Gradient (HMAC-verified webhooks).
          `outbound` - Gradient calls the forge (CI status reports).
        '';
      };

      forge_type = mkOption {
        type = types.enum [ "gitea" "forgejo" "gitlab" "github" ];
        description = ''
          Which forge this integration targets. For inbound integrations this
          is display metadata only - a single inbound row can serve
          Gitea/Forgejo/GitLab via the forge path segment of the webhook URL.

          `github` requires `installation_id` (no secret/token/endpoint); it
          provisions the linked GitHub App installation in place of those
          credentials. GitHub rows are also auto-created when the App is
          installed on the org, so a declared one is reconciled additively.
        '';
      };

      installation_id = mkOption {
        type = types.nullOr types.int;
        default = null;
        description = ''
          GitHub App installation id (trailing number in the installation URL).
          Required when `forge_type = "github"`, ignored otherwise.
        '';
      };

      account_login = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = "GitHub account login for the installation; naming metadata only.";
      };

      secret_file = mkOption {
        type = types.nullOr types.path;
        default = null;
        description = ''
          Path to a file containing the HMAC signing secret for inbound
          integrations. Loaded as a systemd credential and encrypted into
          the database at startup. Ignored for outbound integrations.
        '';
      };

      endpoint_url = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = ''
          Base URL of the forge API for outbound integrations
          (e.g. `https://gitea.example.com`). Ignored for inbound.
        '';
      };

      access_token_file = mkOption {
        type = types.nullOr types.path;
        default = null;
        description = ''
          Path to a file containing the forge API token for outbound
          integrations. Loaded as a systemd credential and encrypted into
          the database at startup. Not used for GitHub outbound - those
          credentials come from the server-configured GitHub App.
        '';
      };

      created_by = mkOption {
        type = types.str;
        description = "Username of the user who created this integration";
      };
    };
  });

  triggerType = types.submodule ({ name, ... }: {
    options = {
      type = mkOption {
        type = types.enum [ "polling" "reporter_push" "reporter_pull_request" "time" ];
        description = "Trigger kind. Drives which `config` shape is expected and how the dispatch loop fires it.";
      };

      integration = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = ''
          Name of an inbound integration in the same organization that backs
          this trigger. Required for `reporter_push` and `reporter_pull_request`;
          ignored for `polling` and `time`. Must name an integration in
          `services.gradient.state.integrations` or a GitHub App row auto-seeded
          when the App is installed on the org.
        '';
      };

      config = mkOption {
        type = types.attrs;
        default = { };
        description = ''
          Type-specific configuration. Shape depends on `type`:

          - `polling`: `{ interval_secs = 300; branch = "main"; }` (minimum 10 seconds; `branch` optional, defaults to remote HEAD)
          - `reporter_push`: `{ branches = [ "main" "release/*" ]; tags = [ ]; releases_only = false; }`
          - `reporter_pull_request`: `{ branches = [ ]; actions = [ "opened" "synchronize" "reopened" ]; require_approval = true; }`
          - `time`: `{ cron = "0 0 2 * * *"; }` (six-field: sec min hour dom mon dow, UTC)

          Empty `branches`/`tags`/`actions` lists mean "match all".

          `require_approval` (PR triggers only, default `true`) parks evaluations
          for PRs from contributors who are not repo writers on the forge until
          a maintainer clicks "Approve and run" on the GitHub check or comments
          `/gradient approve` (or `/gradient run`) on the PR. Set to `false` to
          disable the gate and run every PR build automatically.
        '';
        example = literalExpression ''
          { interval_secs = 60; }
        '';
      };

      active = mkOption {
        type = types.bool;
        default = true;
        description = "Whether the trigger is active. Inactive triggers are stored but never fire.";
      };
    };
  });

  actionType = types.submodule {
    options = {
      name = mkOption {
        type = types.str;
        description = ''
          Action name unique within the project. The provisioner upserts on
          (project_id, name); changing this string creates a new action and
          deletes the old one on next reconciliation.
        '';
      };

      type = mkOption {
        type = types.enum [ "send_mail" "send_web_request" "forge_status_report" "open_pr" ];
        description = "Action kind. Drives which `config` shape is expected.";
      };

      active = mkOption {
        type = types.bool;
        default = true;
        description = "Whether the action is active. Inactive actions are stored but never fire.";
      };

      events = mkOption {
        type = types.listOf types.str;
        default = [];
        description = ''
          Events the action subscribes to. Must be empty for
          `forge_status_report` (status events are derived from build state).
        '';
      };

      config = mkOption {
        type = types.attrs;
        description = ''
          Type-specific configuration. Shape depends on `type`:

          - `send_mail`: `{ recipients = [ "ops@example.com" ]; subject_template = null; }`
          - `send_web_request`: `{ url = "https://hooks.example.com/gradient"; token_file = "/etc/gradient/secrets/<name>-token"; }`
          - `forge_status_report`: `{ integration = "gitea-prod"; }` (name of an outbound integration in the same organization)
          - `open_pr`: opens a pull request on the forge with the result of a
            generator (currently `flake_lock`, which updates `flake.lock`).
            Fields:
            - `integration` (string): name of an outbound integration in the
              same organization, same convention as `forge_status_report`.
            - `generator` (string, default `"flake_lock"`): which change
              generator produces the PR contents.
            - `granularity` (string, default `"per_run"`): one of `"per_run"`
              (a single PR with every input update) or `"per_input"` (one PR
              per updated input).
            - `verify_gate` (string, default `"build"`): one of `"none"`,
              `"eval"` or `"build"`. Gates PR creation on the generated change
              passing the named stage.
            - `branch_pattern` (string, default
              `"gradient/flake-lock-update"`): branch name the PR is opened
              from. For `per_input` granularity it must contain the `{input}`
              placeholder, which is substituted with each input name.
            - `title_template` (string, optional): template for the PR title.
            - `body_template` (string, optional): template for the PR body.
            - `update_existing` (bool, default `true`): when an open PR for the
              same branch already exists, force-push the new contents to it
              instead of opening a duplicate.

          For `send_web_request`, omit `token_file` to send unauthenticated
          requests. When set, the token is read from the systemd credential
          file `gradient_action_''${name}_token` and stored encrypted with
          the server's crypt key.
        '';
        example = literalExpression ''
          { recipients = [ "ops@example.com" ]; }
        '';
      };
    };
  };

  cacheMemberType = types.submodule {
    options = {
      user = mkOption {
        type = types.str;
        description = "Username (resolved at provision time).";
      };
      role = mkOption {
        type = types.str;
        description = "Role name (built-in `Admin`/`Write`/`View` or a custom role declared on this cache).";
      };
    };
  };

  orgMemberType = types.submodule {
    options = {
      user = mkOption {
        type = types.str;
        description = ''
          Username to grant membership to. Resolved at provision time;
          if the user does not yet exist, the membership is recorded as
          pending and applied automatically when the user later registers
          (`POST /user`) or signs in via OIDC for the first time.
        '';
      };
      role = mkOption {
        type = types.str;
        description = ''
          Role name. Either a built-in (`Admin`/`Write`/`View`) or a
          custom org role declared under
          `services.gradient.state.roles` for the same organization.
        '';
      };
    };
  };

  cacheRoleType = types.submodule {
    options = {
      name = mkOption {
        type = types.str;
        description = "Custom role name (must not collide with built-ins).";
      };
      permissions = mkOption {
        type = types.listOf types.str;
        description = "Cache capability identifiers (camelCase). One of: viewCache, readStore, writeStore, manageCacheSettings, manageCacheKeys, manageCacheUpstreams, manageCacheMembers, manageCacheRoles, manageCacheSubscriptions, deleteCache.";
      };
    };
  };

  cacheType = types.submodule ({ config, name, ... }: {
    options = {
      name = mkOption {
        type = types.str;
        default = name;
        defaultText = "<attrset key>";
        description = "Unique name for the cache";
      };

      display_name = mkOption {
        type = types.str;
        default = config.name;
        defaultText = "config.name";
        description = "Display name for the cache";
      };

      description = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = "Description of the cache";
      };

      active = mkOption {
        type = types.bool;
        default = true;
        description = "Whether the cache is active";
      };

      priority = mkOption {
        type = types.ints.positive;
        default = 10;
        description = "Priority of the cache (higher is more important)";
      };

      local_priority = mkOption {
        type = types.nullOr types.int;
        default = null;
        description = ''
          Alternate Priority advertised in nix-cache-info to clients whose
          IP is in `services.gradient.settings.localIps`. Null (or 0)
          disables the override.
        '';
      };

      max_storage_gb = mkOption {
        type = types.ints.unsigned;
        default = 0;
        description = ''
          Max storage for this cache in GB. When all writable caches for an
          org have less than 10 MiB headroom, new evaluations park in Waiting.
          0 = unlimited.
        '';
      };

      signing_key_file = mkOption {
        type = types.str;
        description = "Path to file containing the Nix cache signing key";
      };

      organizations = mkOption {
        type = types.listOf types.str;
        default = [ ];
        description = "List of organization names that can use this cache";
      };

      upstreams = mkOption {
        type = types.listOf upstreamType;
        default = [{
          type = "external";
          display_name = "cache.nixos.org";
          url = "https://cache.nixos.org";
          public_key = "cache.nixos.org-1:6NCHdD59X431o0gWypbMrAURkbJ16ZPMQFGspcDShjY=";
        }];

        description = "List of upstream caches (internal Gradient caches or external Nix binary caches) to use as substituters";
        example = literalExpression ''
          [
            {
              type = "external";
              display_name = "cache.nixos.org";
              url = "https://cache.nixos.org";
              public_key = "cache.nixos.org-1:6NCHdD59X431o0gWypbMrAURkbJ16ZPMQFGspcDShjY=";
            }
            {
              type = "internal";
              cache_name = "other-cache";
              mode = "ReadOnly";
            }
          ]
        '';
      };

      members = mkOption {
        type = types.listOf cacheMemberType;
        default = [];
        description = "Users with direct role assignments on this cache.";
      };

      roles = mkOption {
        type = types.listOf cacheRoleType;
        default = [];
        description = "Custom roles available on this cache.";
      };

      public = mkOption {
        type = types.bool;
        default = false;
        description = "Whether the cache is public (available to all organizations)";
      };

      created_by = mkOption {
        type = types.str;
        description = "Username of the user who created this cache";
      };
    };
  });

  workerType = types.submodule ({ name, ... }: {
    options = {
      display_name = mkOption {
        type = types.str;
        default = name;
        defaultText = "<attrset key>";
        description = "Display name for the worker";
      };

      worker_id = mkOption {
        type = types.str;
        description = "Worker identity string. Must match GRADIENT_WORKER_ID on the worker machine.";
        example = "123e4567-e89b-12d3-a456-426614174000";
      };

      url = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = "WebSocket URL where the worker accepts incoming server connections. When set, the server connects outbound to this URL. Leave empty for worker-initiated connections.";
        example = "wss://worker.example.com/proto";
      };

      organizations = mkOption {
        type = types.listOf types.str;
        default = [ ];
        description = ''
          Organizations this worker is registered under. The provisioner
          creates one `worker_registration` row per
          (worker_id, organization) pair so the same physical worker can
          serve builds for multiple organizations from a single state
          entry. For a base worker, lists organizations to pre-enable;
          may be empty. Non-base workers must list at least one
          organization (enforced by the server at state-apply time).
        '';
        example = [ "acme-corp" "globex" ];
      };

      token_file = mkOption {
        type = types.path;
        description = "Path to a file containing the authentication token for this worker";
      };

      created_by = mkOption {
        type = types.str;
        description = "Username of the user who created this worker registration";
      };

      enable_fetch = mkOption {
        type = types.bool;
        default = true;
        description = "Server-side gate for the worker's `fetch` capability for this registration. When false, the negotiated capability set excludes fetch.";
      };

      enable_eval = mkOption {
        type = types.bool;
        default = true;
        description = "Server-side gate for the worker's `eval` capability for this registration.";
      };

      enable_build = mkOption {
        type = types.bool;
        default = true;
        description = "Server-side gate for the worker's `build` capability for this registration.";
      };

      base_worker = mkOption {
        type = types.bool;
        default = false;
        description = "When true this entry is a base worker (server-level, available to every organization) rather than a per-organization registration. `organizations` then lists organizations to pre-enable.";
      };

      authorize_against = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = "Optional fixed UUID identity a base worker authenticates as, instead of the per-organization challenge. Ignored for non-base workers.";
        example = "123e4567-e89b-12d3-a456-426614174000";
      };

      enabled = mkOption {
        type = types.bool;
        default = true;
        description = "Global enable for a base worker. When false the base worker is unavailable to every organization. Ignored for non-base workers.";
      };
    };
  });

  apiKeyType = types.submodule ({ name, ... }: {
    options = {
      name = mkOption {
        type = types.str;
        default = name;
        defaultText = "<attrset key>";
        description = "Name of the API key";
      };

      key_file = mkOption {
        type = types.str;
        description = ''
          Path to a file containing the lowercase 64-char SHA-256 hex digest
          of the API token (without the `GRAD` prefix). The server stores API
          keys hashed; this hash is what's compared against the digest of the
          incoming bearer token.

          Generate one with:
          `printf %s "$TOKEN" | sha256sum | cut -d' ' -f1 > /etc/gradient/secrets/<name>`
        '';
      };

      owned_by = mkOption {
        type = types.str;
        description = "Username of the user who owns this API key";
      };

      permissions = mkOption {
        type = types.listOf types.str;
        description = ''
          Capability identifiers (camelCase) the API key grants. Must be
          non-empty. The full catalogue is defined in
          `gradient_core::permissions::Permission` and exposed at runtime via
          `GET /user/keys/permissions`. Common identifiers include
          `viewOrg`, `triggerEvaluation`, `editProject`, `manageMembers`.
        '';
        example = [ "viewOrg" "triggerEvaluation" ];
      };

      organization = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = ''
          Optional organization name to pin the key to. When set, the key is
          rejected for every other organization (the request looks identical
          to "not a member"). When null, the key works in any org the owning
          user is a member of.
        '';
      };
    };
  });

  roleType = types.submodule ({ name, ... }: {
    options = {
      name = mkOption {
        type = types.str;
        default = name;
        defaultText = "<attrset key>";
        description = ''
          Name of the role. Must not collide with the built-in role names
          (`Admin`, `Write`, `View`) and must be unique within its
          organization. State-managed roles cannot be modified via the
          role-management API.
        '';
      };

      organization = mkOption {
        type = types.str;
        description = ''
          Organization the role belongs to. State-managed roles are always
          org-scoped - there is no way to define a global state-managed
          role.
        '';
      };

      permissions = mkOption {
        type = types.listOf types.str;
        description = ''
          Capability identifiers (camelCase) the role grants. Must be
          non-empty. See `apiKeyType.permissions` for the catalogue.
        '';
        example = [ "viewOrg" "triggerEvaluation" ];
      };

      oidc_group = mkOption {
        type = types.listOf types.str;
        default = [];
        description = ''
          OIDC group claims that grant this role on login. A user whose
          `groups` claim contains any listed group is granted this role in
          the role's organization. Grants are additive - they never remove a
          membership. Requires the `groups` scope on the OIDC client.
        '';
        example = [ "platform-team" "ops" ];
      };

      scim_group = mkOption {
        type = types.listOf types.str;
        default = [];
        description = ''
          SCIM group names that grant this role. A user the IdP adds to a
          listed SCIM group is granted this role in the role's organization.
          Grants are additive; removal from the group removes the membership.
        '';
        example = [ "acme-eng" ];
      };
    };
  });

  stateType = types.submodule {
    options = {
      users = mkOption {
        type = types.attrsOf userType;
        default = { };
        description = "Attribute set of users to create, keyed by username";
      };

      organizations = mkOption {
        type = types.attrsOf organizationType;
        default = { };
        description = "Attribute set of organizations to create, keyed by name";
      };

      projects = mkOption {
        type = types.attrsOf projectType;
        default = { };
        description = "Attribute set of projects to create, keyed by name";
      };

      integrations = mkOption {
        type = types.attrsOf integrationType;
        default = { };
        description = ''
          Attribute set of per-organization forge integrations, keyed by name.
          Each entry inserts a row into `integration`. For inbound integrations,
          `secret_file` is read as a systemd credential and stored encrypted.
          For outbound integrations, `access_token_file` is similarly encrypted.
        '';

        example = literalExpression ''
          {
            acme-prod-inbound = {
              organization = "acme-corp";
              kind = "inbound";
              forge_type = "gitea";
              secret_file = "/etc/gradient/secrets/acme-inbound-hmac";
              created_by = "alice";
            };
            acme-status-reports = {
              organization = "acme-corp";
              kind = "outbound";
              forge_type = "gitea";
              endpoint_url = "https://gitea.example.com";
              access_token_file = "/etc/gradient/secrets/acme-gitea-token";
              created_by = "alice";
            };
            acme-github-out = {
              organization = "acme-corp";
              kind = "outbound";
              forge_type = "github";
              installation_id = 12345678;
              account_login = "acme-corp";
              created_by = "alice";
            };
          }
        '';
      };

      caches = mkOption {
        type = types.attrsOf cacheType;
        default = { };
        description = "Attribute set of caches to create, keyed by name";
      };

      roles = mkOption {
        type = types.attrsOf roleType;
        default = { };
        description = ''
          Attribute set of state-managed custom roles, keyed by role name.
          Each entry creates a custom role in the specified organization with
          the given permission set. Managed roles cannot be modified or
          deleted through the API - only this state file can change them.
        '';
      };

      api_keys = mkOption {
        type = types.attrsOf apiKeyType;
        default = { };
        description = "Attribute set of API keys to create, keyed by name";
      };

      workers = mkOption {
        type = types.attrsOf workerType;
        default = { };
        description = ''
          Attribute set of worker registrations, keyed by worker_id.
          Each entry inserts a row into worker_registration so the worker
          can authenticate via challenge-response. The token is read from
          token_file, hashed, and stored - the plaintext is never persisted.
        '';

        example = literalExpression ''
          {
            builder-1 = {
              display_name = "Primary Build Server";
              organizations = [ "acme-corp" ];
              token_file = "/etc/gradient/secrets/builder-1-token";
              created_by = "alice";
            };
          }
        '';
      };
    };
  };

in
{
  options.services.gradient = {
    state = mkOption {
      type = stateType;
      default = { };
      description = "Gradient state configuration for users, organizations, projects, and caches";
      example = literalExpression ''
        {
          users = {
            alice = {
              name = "Alice Johnson";
              email = "alice@example.com";
              password_file = "/etc/gradient/secrets/alice_password";
              email_verified = true;
              superuser = true;
            };
          };
          organizations = {
            acme-corp = {
              display_name = "ACME Corporation";
              description = "Main development organization";
              private_key_file = "/etc/gradient/secrets/acme_ssh_key";
              created_by = "alice";
            };
          };
          projects = {
            web-app = {
              organization = "acme-corp";
              display_name = "Web Application";
              description = "Main web application";
              repository = "https://github.com/acme-corp/web-app.git";
              wildcard = "nixosConfigurations.*.config.system.build.toplevel";
              active = true;
              concurrency = "hard_abort";
              created_by = "alice";
              triggers = [
                {
                  type = "polling";
                  config = { interval_secs = 300; };
                }
              ];
            };
          };
          caches = {
            main-cache = {
              display_name = "Main Binary Cache";
              description = "Primary binary cache";
              signing_key_file = "/etc/gradient/secrets/main_cache_key";
              organizations = [ "acme-corp" ];
              created_by = "alice";
            };
          };
          roles = {
            releaser = {
              organization = "acme-corp";
              permissions = [ "viewOrg" "triggerEvaluation" ];
            };
          };
          api_keys = {
            ci-runner = {
              key_file = "/etc/gradient/secrets/ci-runner";
              owned_by = "alice";
              permissions = [ "viewOrg" "triggerEvaluation" ];
              organization = "acme-corp";
            };
          };
        }
      '';
    };
  };

  config.assertions =
    let
      bad = flatten (mapAttrsToList (pName: p:
        mapAttrsToList (iName: o: {
          project = pName;
          input = iName;
          valid = (o.url != null) != o.keep_url;
        }) p.flake_input_overrides
      ) config.services.gradient.state.projects);
      invalid = filter (b: !b.valid) bad;

      badActions = flatten (mapAttrsToList (pName: p:
        map (a: {
          project = pName;
          action = a.name;
          valid = !(a.type == "forge_status_report" && a.events != []);
        }) p.actions
      ) config.services.gradient.state.projects);
      invalidActions = filter (b: !b.valid) badActions;
    in
    map (b: {
      assertion = false;
      message = ''
        services.gradient.state.projects.${b.project}.flake_input_overrides.${b.input}: \
        exactly one of `url` (string) or `keep_url = true` must be set.
      '';
    }) invalid
    ++ map (b: {
      assertion = false;
      message = ''
        services.gradient.state.projects.${b.project}.actions.${b.action}: \
        forge_status_report actions cannot declare custom `events`.
      '';
    }) invalidActions;
}
