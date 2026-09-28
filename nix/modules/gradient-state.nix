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
        description = ''
          Upstream type: `internal` (another Gradient cache) or `external` (a Nix binary cache URL).
        '';
      };

      cache_name = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = ''
          Name of the internal Gradient cache to use. Required for `internal` upstreams.
        '';
      };

      display_name = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = "Display name of the upstream. Required for `external` upstreams.";
      };

      mode = mkOption {
        type = types.enum [ "ReadWrite" "ReadOnly" "WriteOnly" ];
        default = "ReadWrite";
        description = ''
          Access mode of an internal upstream. External upstreams are always read-only.
        '';
      };

      url = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = "URL of the external Nix binary cache. Required for `external` upstreams.";
      };

      public_key = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = ''
          Public key of the external Nix binary cache. Required for `external` upstreams.
        '';
      };
    };
  };

  userType = types.submodule ({ config, name, ... }: {
    options = {
      username = mkOption {
        type = types.str;
        default = name;
        defaultText = "<attrset key>";
        description = "Unique user name.";
      };

      name = mkOption {
        type = types.str;
        default = config.username;
        defaultText = "config.username";
        description = "Full name of the user.";
      };

      email = mkOption {
        type = types.str;
        description = "Email address of the user.";
      };

      password_file = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = ''
          File containing the hashed password. `null` creates the account without a local password,
          so an OIDC login with the same email can claim it.
        '';
      };

      email_verified = mkOption {
        type = types.bool;
        default = true;
        description = "Whether the user's email address is verified.";
      };

      superuser = mkOption {
        type = types.bool;
        default = false;
        description = "Whether the user is a superuser.";
      };
    };
  });

  projectType = types.submodule ({ config, name, ... }: {
    options = {
      name = mkOption {
        type = types.str;
        default = name;
        defaultText = "<attrset key>";
        description = "Unique project name.";
      };

      display_name = mkOption {
        type = types.str;
        default = config.name;
        defaultText = "config.name";
        description = "Display name of the project.";
      };

      id = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = ''
          Project UUID. `null` lets the server generate one. Set it so a worker's
          {option}`services.gradient.worker.peersFile` can reference the project as `<id>:<token>`
          in a fully declarative deployment. Only applied on creation; a value conflicting with an
          existing project is rejected. Generate one with {command}`uuidgen`.
        '';
      };

      description = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = "Description of the project.";
      };

      private_key_file = mkOption {
        type = types.str;
        description = "File containing the SSH private key used for Git access.";
      };

      public = mkOption {
        type = types.bool;
        default = false;
        description = "Whether the project is visible to all users.";
      };

      hide_build_requests = mkOption {
        type = types.bool;
        default = false;
        description = ''
          Whether to hide the project's automatic `build-request` task from task listings in the web
          UI. The task keeps receiving evaluations from {command}`gradient build`.
        '';
      };

      created_by = mkOption {
        type = types.str;
        description = "User name of the project's creator.";
      };

      members = mkOption {
        type = types.listOf projectMemberType;
        default = [];
        example = literalExpression ''
          [
            { user = "alice"; role = "Admin"; }
            { user = "bob";   role = "Write"; }
            { user = "carol"; role = "releaser"; }
          ]
        '';
        description = ''
          Users with roles on this project. An empty list keeps the legacy behaviour: `created_by`
          becomes Admin and no other memberships are reconciled.

          A non-empty list is the source of truth: memberships not listed are revoked on the next
          state apply, and `created_by` is not made Admin implicitly. Members referring to users
          that do not exist yet are applied once the user registers or first signs in through OIDC.
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
          Flake reference overriding this input. `null` together with `keep_url` force-updates the
          input from the URL declared in the task's {file}`flake.nix`.
        '';
      };
      keep_url = mkOption {
        type = types.bool;
        default = false;
        description = ''
          Whether to force-update this input from its flake-declared URL. Mutually exclusive with
          `url`; exactly one of the two must be set.
        '';
      };
    };
  });

  taskType = types.submodule ({ config, name, ... }: {
    options = {
      name = mkOption {
        type = types.str;
        default = name;
        defaultText = "<attrset key>";
        description = "Unique task name.";
      };

      project = mkOption {
        type = types.str;
        description = "Name of the project the task belongs to.";
      };

      display_name = mkOption {
        type = types.str;
        default = config.name;
        defaultText = "config.name";
        description = "Display name of the task.";
      };

      description = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = "Description of the task.";
      };

      repository = mkOption {
        type = types.str;
        description = "Git repository URL of the task.";
      };

      wildcard = mkOption {
        type = types.str;
        default = "packages.x86_64-linux.*";
        description = "Branch or branch pattern to evaluate.";
      };

      active = mkOption {
        type = types.bool;
        default = true;
        description = "Whether the task is active.";
      };

      keep_evaluations = mkOption {
        type = types.ints.positive;
        default = 1;
        description = ''
          Number of finished evaluations kept for metrics and history, regardless of outcome. Older
          ones are garbage collected, and collection pauses while an evaluation runs. Must be at
          least 1 and is capped by {option}`services.gradient.eval.maxKeep`.
        '';
      };

      sign_cache = mkOption {
        type = types.bool;
        default = true;
        description = ''
          Whether to sign the narinfo of outputs pushed by this task. Unsigned outputs are not
          trusted by external Nix clients, which keeps them private even in a public cache. A path
          also produced by a signing task is still signed.
        '';
      };

      concurrency = mkOption {
        type = types.enum [ "hard_abort" "soft_abort" "skip" "all" ];
        default = "soft_abort";
        description = ''
          What a new trigger event does while an evaluation is running.

          - `hard_abort` cancels the running evaluation and its builds and starts a new one.
          - `soft_abort` marks the running evaluation aborted so the new one becomes canonical,
            but lets its builds finish; their outputs flow into the new evaluation.
          - `skip` discards the new event.
          - `all` runs the new evaluation alongside the running one.
        '';
      };

      triggers = mkOption {
        type = types.nullOr (types.listOf triggerType);
        default = null;
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
        description = ''
          Evaluation triggers of the task: polling, forge push, forge pull request or cron schedule.
          `null` leaves existing triggers untouched. An empty list is rejected, since every task
          needs a trigger.

          New tasks get a polling trigger every 300 seconds; declaring triggers replaces it.
        '';
      };

      flake_input_overrides = mkOption {
        type = types.attrsOf flakeInputOverrideType;
        default = {};
        example = literalExpression ''
          {
            nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
            flake-utils.keep_url = true;
          }
        '';
        description = ''
          Overrides applied when fetching flake inputs, keyed by input name. An empty set uses
          {file}`flake.lock` as is.
        '';
      };

      actions = mkOption {
        type = types.listOf actionType;
        default = [];
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
        description = ''
          Task actions: email notifications, web requests, forge status reports and pull request
          automation. Actions missing on the next state apply are removed, matched by `name`.

          Token files of `send_web_request` actions must live at the systemd credential path
          `''${GRADIENT_CREDENTIALS_DIR}/gradient_action_''${name}_token`.
        '';
      };

      created_by = mkOption {
        type = types.str;
        description = "User name of the task's creator.";
      };
    };
  });

  integrationType = types.submodule ({ config, name, ... }: {
    options = {
      name = mkOption {
        type = types.str;
        default = name;
        defaultText = "<attrset key>";
        description = "Integration name, unique per project and kind.";
      };

      display_name = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = "Display name of the integration. `null` uses `name`.";
      };

      project = mkOption {
        type = types.str;
        description = "Name of the project the integration belongs to.";
      };

      kind = mkOption {
        type = types.enum [ "inbound" "outbound" ];
        description = ''
          Direction of the integration: `inbound` for HMAC-verified webhooks from the forge,
          `outbound` for CI status reports to the forge.
        '';
      };

      forge_type = mkOption {
        type = types.enum [ "gitea" "forgejo" "gitlab" "github" ];
        description = ''
          Forge this integration targets. For inbound integrations it is display metadata only,
          since one inbound row serves Gitea, Forgejo and GitLab through the webhook URL's forge
          segment.

          `github` requires `installation_id` instead of a secret, token or endpoint, and provisions
          the linked GitHub App installation. GitHub rows are also created when the App is installed
          on the project, so a declared one is reconciled additively.
        '';
      };

      installation_id = mkOption {
        type = types.nullOr types.int;
        default = null;
        description = ''
          GitHub App installation ID, the trailing number of the installation URL. Required for
          `forge_type = "github"`, ignored otherwise.
        '';
      };

      account_login = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = "GitHub account login of the installation, used for naming only.";
      };

      secret_file = mkOption {
        type = types.nullOr types.path;
        default = null;
        description = ''
          File containing the HMAC signing secret of an inbound integration. It is loaded as a
          systemd credential and stored encrypted. Ignored for outbound integrations.
        '';
      };

      endpoint_url = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = ''
          Base URL of the forge API for outbound integrations, such as `https://gitea.example.com`.
          Ignored for inbound integrations.
        '';
      };

      access_token_file = mkOption {
        type = types.nullOr types.path;
        default = null;
        description = ''
          File containing the forge API token of an outbound integration. It is loaded as a systemd
          credential and stored encrypted. Not used for GitHub, whose credentials come from
          {option}`services.gradient.githubApp`.
        '';
      };

      created_by = mkOption {
        type = types.str;
        description = "User name of the integration's creator.";
      };
    };
  });

  triggerType = types.submodule ({ name, ... }: {
    options = {
      type = mkOption {
        type = types.enum [ "polling" "reporter_push" "reporter_pull_request" "time" ];
        description = ''
          Trigger kind, which determines the expected `config` and how the trigger fires.
        '';
      };

      integration = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = ''
          Name of an inbound integration in the same project backing this trigger. Required for
          `reporter_push` and `reporter_pull_request`, ignored for `polling` and `time`. It must
          name an integration in {option}`services.gradient.state.integrations` or a GitHub App
          installation of the project.
        '';
      };

      config = mkOption {
        type = types.attrs;
        default = { };
        example = literalExpression ''
          { interval_secs = 60; }
        '';
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
          Action name, unique within the task. Renaming creates a new action and deletes the old one
          on the next state apply.
        '';
      };

      type = mkOption {
        type = types.enum [ "send_mail" "send_web_request" "forge_status_report" "open_pr" ];
        description = "Action kind, which determines the expected `config`.";
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
          Events the action subscribes to. Must be empty for `forge_status_report`, whose events
          derive from build state.
        '';
      };

      config = mkOption {
        type = types.attrs;
        example = literalExpression ''
          { recipients = [ "ops@example.com" ]; }
        '';
        description = ''
          Type-specific configuration. Shape depends on `type`:

          - `send_mail`: `{ recipients = [ "ops@example.com" ]; subject_template = null; }`
          - `send_web_request`: `{ url = "https://hooks.example.com/gradient"; token_file = "/etc/gradient/secrets/<name>-token"; }`
          - `forge_status_report`: `{ integration = "gitea-prod"; }` (name of an outbound integration in the same project)
          - `open_pr`: opens a pull request on the forge with the result of a
            generator (currently `flake_lock`, which updates `flake.lock`).
            Fields:
            - `integration` (string): name of an outbound integration in the
              same project, same convention as `forge_status_report`.
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
      };
    };
  };

  cacheMemberType = types.submodule {
    options = {
      user = mkOption {
        type = types.str;
        description = "User name, resolved when the state is applied.";
      };
      role = mkOption {
        type = types.str;
        description = ''
          Role name: a built-in `Admin`, `Write` or `View`, or a custom role of this cache.
        '';
      };
    };
  };

  projectMemberType = types.submodule {
    options = {
      user = mkOption {
        type = types.str;
        description = ''
          User name to grant membership to. If the user does not exist yet, the membership is
          applied once they register or first sign in through OIDC.
        '';
      };
      role = mkOption {
        type = types.str;
        description = ''
          Role name: a built-in `Admin`, `Write` or `View`, or a custom role of the same project
          declared in {option}`services.gradient.state.roles`.
        '';
      };
    };
  };

  cacheRoleType = types.submodule {
    options = {
      name = mkOption {
        type = types.str;
        description = "Custom role name, distinct from the built-in roles.";
      };
      permissions = mkOption {
        type = types.listOf types.str;
        description = ''
          Cache permissions granted by the role: `viewCache`, `readStore`, `writeStore`,
          `manageCacheSettings`, `manageCacheKeys`, `manageCacheUpstreams`, `manageCacheMembers`,
          `manageCacheRoles`, `manageCacheSubscriptions` or `deleteCache`.
        '';
      };
    };
  };

  cacheType = types.submodule ({ config, name, ... }: {
    options = {
      name = mkOption {
        type = types.str;
        default = name;
        defaultText = "<attrset key>";
        description = "Unique cache name.";
      };

      display_name = mkOption {
        type = types.str;
        default = config.name;
        defaultText = "config.name";
        description = "Display name of the cache.";
      };

      description = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = "Description of the cache.";
      };

      active = mkOption {
        type = types.bool;
        default = true;
        description = "Whether the cache is active.";
      };

      priority = mkOption {
        type = types.ints.positive;
        default = 10;
        description = "Priority of the cache; higher is preferred.";
      };

      local_priority = mkOption {
        type = types.nullOr types.int;
        default = null;
        description = ''
          Priority advertised in {file}`nix-cache-info` to clients within
          {option}`services.gradient.http.localIps`. `null` or `0` disables the override.
        '';
      };

      max_storage_gb = mkOption {
        type = types.ints.unsigned;
        default = 0;
        description = ''
          Storage limit of the cache in GB. When every writable cache of a project has less than 10
          MiB left, new evaluations wait. `0` disables the limit.
        '';
      };

      signing_key_file = mkOption {
        type = types.str;
        description = "File containing the Nix cache signing key.";
      };

      projects = mkOption {
        type = types.listOf types.str;
        default = [ ];
        description = "Names of the projects using this cache.";
      };

      upstreams = mkOption {
        type = types.listOf upstreamType;
        default = [{
          type = "external";
          display_name = "cache.nixos.org";
          url = "https://cache.nixos.org";
          public_key = "cache.nixos.org-1:6NCHdD59X431o0gWypbMrAURkbJ16ZPMQFGspcDShjY=";
        }];
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
        description = ''
          Upstream caches used as substituters: internal Gradient caches or external Nix binary
          caches.
        '';
      };

      members = mkOption {
        type = types.listOf cacheMemberType;
        default = [];
        description = "Users with direct roles on this cache.";
      };

      roles = mkOption {
        type = types.listOf cacheRoleType;
        default = [];
        description = "Custom roles of this cache.";
      };

      public = mkOption {
        type = types.bool;
        default = false;
        description = "Whether the cache is available to all projects.";
      };

      created_by = mkOption {
        type = types.str;
        description = "User name of the cache's creator.";
      };
    };
  });

  workerType = types.submodule ({ name, ... }: {
    options = {
      display_name = mkOption {
        type = types.str;
        default = name;
        defaultText = "<attrset key>";
        description = "Display name of the worker.";
      };

      worker_id = mkOption {
        type = types.str;
        example = "123e4567-e89b-12d3-a456-426614174000";
        description = ''
          Worker identity. Must match {option}`services.gradient.worker.id` on the worker host.
        '';
      };

      url = mkOption {
        type = types.nullOr types.str;
        default = null;
        example = "wss://worker.example.com/proto";
        description = ''
          WebSocket URL on which the worker accepts server connections. When set, the server
          connects to the worker; empty lets the worker connect to the server.
        '';
      };

      projects = mkOption {
        type = types.listOf types.str;
        default = [ ];
        example = [ "acme-corp" "globex" ];
        description = ''
          Projects the worker is registered under, one registration per project, so a single worker
          can serve several projects. For a base worker this lists projects to enable up front and
          may be empty; other workers need at least one.
        '';
      };

      token_file = mkOption {
        type = types.path;
        description = "File containing the worker's authentication token.";
      };

      created_by = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = ''
          User name of the registration's creator. `null` leaves it unattributed, as for a worker a
          host provisions for itself.
        '';
      };

      enable_fetch = mkOption {
        type = types.bool;
        default = true;
        description = ''
          Whether the server grants this registration the worker's `fetch` capability.
        '';
      };

      enable_eval = mkOption {
        type = types.bool;
        default = true;
        description = "Whether the server grants this registration the worker's `eval` capability.";
      };

      enable_build = mkOption {
        type = types.bool;
        default = true;
        description = ''
          Whether the server grants this registration the worker's `build` capability.
        '';
      };

      base_worker = mkOption {
        type = types.bool;
        default = true;
        description = ''
          Whether this is a base worker available to every project instead of a per-project
          registration. `projects` then lists projects to enable up front.
        '';
      };

      authorize_against = mkOption {
        type = types.nullOr types.str;
        default = null;
        example = "123e4567-e89b-12d3-a456-426614174000";
        description = ''
          UUID a base worker authenticates as instead of the per-project challenge. Ignored for
          other workers.
        '';
      };

      auto_enable = mkOption {
        type = types.bool;
        default = true;
        description = ''
          Whether every new project enables this base worker on creation instead of opting in
          through the web UI. Ignored for other workers.
        '';
      };

      enabled = mkOption {
        type = types.bool;
        default = true;
        description = "Whether the base worker is available at all. Ignored for other workers.";
      };
    };
  });

  apiKeyType = types.submodule ({ name, ... }: {
    options = {
      name = mkOption {
        type = types.str;
        default = name;
        defaultText = "<attrset key>";
        description = "Name of the API key.";
      };

      key_file = mkOption {
        type = types.str;
        description = ''
          File containing the lowercase hex SHA-256 digest of the API token, without its `GRAD`
          prefix. The server only stores and compares hashes. Generate it with `printf %s "$TOKEN" |
          sha256sum | cut -d' ' -f1`.
        '';
      };

      owned_by = mkOption {
        type = types.str;
        description = "User name of the key's owner.";
      };

      permissions = mkOption {
        type = types.listOf types.str;
        example = [ "viewProject" "triggerEvaluation" ];
        description = ''
          Permissions granted by the key, as camelCase identifiers such as `viewProject`,
          `triggerEvaluation`, `editTask` or `manageMembers`. Must not be empty. `GET
          /user/keys/permissions` lists them all.
        '';
      };

      project = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = ''
          Project the key is restricted to. `null` allows every project the owner is a member of.
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
          Role name, distinct from the built-in `Admin`, `Write` and `View` and unique within its
          project. Roles managed here cannot be changed through the API.
        '';
      };

      project = mkOption {
        type = types.str;
        description = "Project the role belongs to. Roles managed here are always project-scoped.";
      };

      permissions = mkOption {
        type = types.listOf types.str;
        example = [ "viewProject" "triggerEvaluation" ];
        description = ''
          Permissions granted by the role, as camelCase identifiers. Must not be empty.
        '';
      };

      oidc_group = mkOption {
        type = types.listOf types.str;
        default = [];
        example = [ "platform-team" "ops" ];
        description = ''
          OIDC groups granting this role on login. A user whose `groups` claim contains a listed
          group gets the role in its project. Grants only add memberships. Requires the `groups`
          scope.
        '';
      };

      scim_group = mkOption {
        type = types.listOf types.str;
        default = [];
        example = [ "acme-eng" ];
        description = ''
          SCIM groups granting this role. Adding a user to a listed group grants the role in its
          project; removing them removes the membership.
        '';
      };
    };
  });

  stateType = types.submodule {
    options = {
      validate = mkOption {
        type = types.bool;
        default = true;
        description = ''
          Whether to validate the generated state at build time with the server's
          `--state-validate`, so schema and reference errors fail the Nix build instead of the first
          server start. No database is touched.
        '';
      };

      delete = mkOption {
        type = types.bool;
        default = true;
        description = "Whether to delete users, projects and caches no longer declared here.";
      };

      users = mkOption {
        type = types.attrsOf userType;
        default = { };
        description = "Users to create, keyed by user name.";
      };

      projects = mkOption {
        type = types.attrsOf projectType;
        default = { };
        description = "Projects to create, keyed by name.";
      };

      tasks = mkOption {
        type = types.attrsOf taskType;
        default = { };
        description = "Tasks to create, keyed by name.";
      };

      integrations = mkOption {
        type = types.attrsOf integrationType;
        default = { };
        example = literalExpression ''
          {
            acme-prod-inbound = {
              project = "acme-corp";
              kind = "inbound";
              forge_type = "gitea";
              secret_file = "/etc/gradient/secrets/acme-inbound-hmac";
              created_by = "alice";
            };
            acme-status-reports = {
              project = "acme-corp";
              kind = "outbound";
              forge_type = "gitea";
              endpoint_url = "https://gitea.example.com";
              access_token_file = "/etc/gradient/secrets/acme-gitea-token";
              created_by = "alice";
            };
            acme-github-out = {
              project = "acme-corp";
              kind = "outbound";
              forge_type = "github";
              installation_id = 12345678;
              account_login = "acme-corp";
              created_by = "alice";
            };
          }
        '';
        description = ''
          Forge integrations per project, keyed by name. Secrets of inbound and tokens of outbound
          integrations are read as systemd credentials and stored encrypted.
        '';
      };

      caches = mkOption {
        type = types.attrsOf cacheType;
        default = { };
        description = "Caches to create, keyed by name.";
      };

      roles = mkOption {
        type = types.attrsOf roleType;
        default = { };
        description = ''
          Custom roles, keyed by role name. They cannot be modified or deleted through the API.
        '';
      };

      api_keys = mkOption {
        type = types.attrsOf apiKeyType;
        default = { };
        description = "API keys to create, keyed by name.";
      };

      workers = mkOption {
        type = types.attrsOf workerType;
        default = { };
        example = literalExpression ''
          {
            builder-1 = {
              display_name = "Primary Build Server";
              projects = [ "acme-corp" ];
              token_file = "/etc/gradient/secrets/builder-1-token";
              created_by = "alice";
            };
          }
        '';
        description = ''
          Worker registrations, keyed by worker ID. The token from `token_file` is stored hashed and
          never persisted in plain text.
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
          projects = {
            acme-corp = {
              display_name = "ACME Corporation";
              description = "Main development project";
              private_key_file = "/etc/gradient/secrets/acme_ssh_key";
              created_by = "alice";
            };
          };
          tasks = {
            web-app = {
              project = "acme-corp";
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
              projects = [ "acme-corp" ];
              created_by = "alice";
            };
          };
          roles = {
            releaser = {
              project = "acme-corp";
              permissions = [ "viewProject" "triggerEvaluation" ];
            };
          };
          api_keys = {
            ci-runner = {
              key_file = "/etc/gradient/secrets/ci-runner";
              owned_by = "alice";
              permissions = [ "viewProject" "triggerEvaluation" ];
              project = "acme-corp";
            };
          };
        }
      '';
      description = "Declarative Gradient state: users, projects, tasks, caches and more.";
    };
  };

  config.assertions = let
    bad = flatten (mapAttrsToList (pName: p:
      mapAttrsToList (iName: o: {
        task = pName;
        input = iName;
        valid = (o.url != null) != o.keep_url;
      }) p.flake_input_overrides
    ) config.services.gradient.state.tasks);
    invalid = filter (b: !b.valid) bad;

    badActions = flatten (mapAttrsToList (pName: p:
      map (a: {
        task = pName;
        action = a.name;
        valid = !(a.type == "forge_status_report" && a.events != []);
      }) p.actions
    ) config.services.gradient.state.tasks);

    invalidActions = filter (b: !b.valid) badActions;
  in map (b: {
    assertion = false;
    message = ''
      services.gradient.state.tasks.${b.task}.flake_input_overrides.${b.input}: \
      exactly one of `url` (string) or `keep_url = true` must be set.
    '';
  }) invalid ++ map (b: {
    assertion = false;
    message = ''
      services.gradient.state.tasks.${b.task}.actions.${b.action}: \
      forge_status_report actions cannot declare custom `events`.
    '';
  }) invalidActions;
}
