/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ lib, config, options, ... }: with lib; let
  renamedStateOptions = [
    { group = "caches"; from = "upstreams"; to = "upstream_caches"; }
    { group = "integrations"; from = "forge_type"; to = "git_host_type"; }
  ];

  stateAlias = from: to: doRename {
    from = [ from ];
    to = [ to ];
    visible = false;
    warn = false;
    use = x: x;
  };

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
          Name of the internal Gradient cache to use. It is required for `internal` upstream
          caches.
        '';
      };

      display_name = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = ''
          Display name of the upstream cache. It is required for `external` upstream caches. `null`
          is using the name of the internal cache.
        '';
      };

      mode = mkOption {
        type = types.enum [ "ReadWrite" "ReadOnly" "WriteOnly" ];
        default = "ReadWrite";
        description = ''
          Access mode of an internal upstream cache. External upstream caches are always read-only.
        '';
      };

      url = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = "URL of the external Nix binary cache. It is required for `external` upstream caches.";
      };

      public_key = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = ''
          Public key of the external Nix binary cache. It is required for `external` upstream
          caches.
        '';
      };

      active = mkOption {
        type = types.bool;
        default = true;
        description = ''
          Whether the upstream cache is active. Gradient is keeping inactive upstream caches but never
          querying them. The UI can toggle this value, and the next server start is restoring it.
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
          File containing the hashed password. `null` is creating the account without a local
          password, and an OIDC login with the same email can claim it.
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
          Project UUID. `null` is letting the server generate one. Set it to reference the project
          as `<id>:<token>` in a worker's {option}`services.gradient.worker.peersFile` in a fully
          declarative deployment. It is only applied on creation, and a value conflicting with an
          existing project is rejected. Generate one with {command}`uuidgen`.
        '';
      };

      description = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = "Description of the project. `null` is leaving it empty.";
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
          UI. The task is still receiving evaluations from {command}`gradient build`.
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
          Users with roles on this project. An empty list is keeping the legacy behaviour, making
          `created_by` Admin and reconciling no other memberships.

          A non-empty list is the source of truth. The next state apply is revoking memberships not
          listed and is not making `created_by` Admin implicitly. Members referring to users not
          existing yet are applied on the user's registration or first OIDC sign-in.
        '';
      };

      teams = mkOption {
        type = types.listOf projectTeamType;
        default = [ ];
        description = ''
          Teams granted on this project. An empty list leaves the project's grants alone; a
          non-empty list is the source of truth and the next state apply removes grants not listed.
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
          Flake reference overriding this input. `null` together with `keep_url` is force-updating
          the input from the URL declared in the task's {file}`flake.nix`.
        '';
      };
      keep_url = mkOption {
        type = types.bool;
        default = false;
        description = ''
          Whether to force-update this input from its flake-declared URL. It is mutually exclusive
          with `url`, and exactly one of the two must be set.
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
        description = "Name of the project owning the task.";
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
        description = "Description of the task. `null` is leaving it empty.";
      };

      repository = mkOption {
        type = types.str;
        description = "Git repository URL of the task.";
      };

      wildcard = mkOption {
        type = types.str;
        default = "packages.x86_64-linux.*";
        description = ''
          Comma-separated Nix attribute paths to evaluate from the flake. A `*` or `#` segment is
          matching any attribute name. A pattern prefixed with `!` is excluding matching paths.
        '';
      };

      active = mkOption {
        type = types.bool;
        default = true;
        description = "Whether the task is active.";
      };

      keep_evaluations = mkOption {
        type = types.ints.positive;
        default = 30;
        description = ''
          Number of finished evaluations kept for metrics and history, regardless of outcome. Older
          ones are garbage collected, and the collection is pausing while an evaluation is active.
          It must be at least 1 and at most {option}`services.gradient.eval.maxKeep`.
        '';
      };

      sign_cache = mkOption {
        type = types.bool;
        default = true;
        description = ''
          Whether to sign the narinfo of outputs pushed by this task. External Nix clients are not
          trusting unsigned outputs, which is keeping them private even in a public cache. A path
          also produced by a signing task is still signed.
        '';
      };

      wait_for_workers = mkOption {
        type = types.bool;
        default = false;
        description = ''
          Whether an evaluation is waiting for a worker when its builds need an architecture or
          system features no connected worker is providing. The server is otherwise aborting such an
          evaluation with a warning naming what is missing.
        '';
      };

      concurrency = mkOption {
        type = types.enum [ "hard_abort" "soft_abort" "skip" "all" ];
        default = "soft_abort";
        description = ''
          Behavior of a new trigger event while an evaluation is running.

          - `hard_abort` is cancelling the running evaluation and its builds and starting a new one.
          - `soft_abort` is marking the running evaluation aborted and making the new one canonical.
            Its builds are finishing, and their outputs are flowing into the new evaluation.
          - `skip` is discarding the new event.
          - `all` is running the new evaluation alongside the current one.
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
          Evaluation triggers of the task: polling, Git host push, Git host pull request or cron schedule.
          `null` is leaving existing triggers untouched, and a new task declared with `null` is
          starting with none. An empty list is rejected.
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
          Overrides applied when fetching flake inputs, one entry per input name. An empty set is
          using {file}`flake.lock` as is.
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
              type = "git_host_status_report";
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
          Task actions: email notifications, web requests, Matrix and Slack messages, Git host status
          reports and pull request automation. Actions missing on the next state apply are removed,
          matched by `name`.

          Secret files of actions (`token_file`, `access_token_file`, `webhook_url_file`) must live
          at the systemd credential path
          `''${GRADIENT_CREDENTIALS_DIR}/gradient_action_''${name}_<field>`.
        '';
      };

      created_by = mkOption {
        type = types.str;
        description = "User name of the task's creator.";
      };
    };
  });

  integrationType = types.submodule ({ config, name, ... }: {
    imports = [ (stateAlias "forge_type" "git_host_type") ];

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
        description = "Display name of the integration. `null` is using `name`.";
      };

      project = mkOption {
        type = types.str;
        description = "Name of the project owning the integration.";
      };

      kind = mkOption {
        type = types.enum [ "inbound" "outbound" ];
        description = ''
          Direction of the integration: `inbound` for HMAC-verified webhooks from the Git host,
          `outbound` for CI status reports to the Git host.
        '';
      };

      git_host_type = mkOption {
        type = types.enum [ "gitea" "forgejo" "gitlab" "github" ];
        description = ''
          Target Git host of this integration. It is display metadata only for inbound
          integrations. One inbound row is serving Gitea, Forgejo and GitLab through the Git host
          segment of the webhook URL.

          `github` is requiring `installation_id` instead of a secret, token or endpoint and is
          provisioning the linked GitHub App installation. Installing the App on the project is
          also creating GitHub rows, and a declared one is merged additively.
        '';
      };

      installation_id = mkOption {
        type = types.nullOr types.int;
        default = null;
        description = ''
          GitHub App installation ID, the trailing number of the installation URL. It is required
          for `git_host_type = "github"` and ignored otherwise.
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
          systemd credential and stored encrypted. Outbound integrations are ignoring it.
        '';
      };

      endpoint_url = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = ''
          Base URL of the Git host API for outbound integrations, such as
          `https://gitea.example.com`. Inbound integrations are ignoring it.
        '';
      };

      access_token_file = mkOption {
        type = types.nullOr types.path;
        default = null;
        description = ''
          File containing the Git host API token of an outbound integration. It is loaded as a systemd
          credential and stored encrypted. GitHub integrations are not using it and are taking their
          credentials from {option}`services.gradient.githubApp`.
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
          Trigger kind, determining the expected `config` and the firing behavior of the trigger.
        '';
      };

      integration = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = ''
          Name of an inbound integration in the same project backing this trigger. It is required
          for `reporter_push` and `reporter_pull_request` and ignored for `polling` and `time`. It
          must name an integration in {option}`services.gradient.state.integrations` or a GitHub App
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
          Type-specific configuration. Its shape is depending on `type`.

          - `polling` is taking `{ interval_secs = 300; branch = "main"; }`. The interval is at
            least 10 seconds, and `branch` is defaulting to the remote HEAD.
          - `reporter_push` is taking
            `{ branches = [ "main" "release/*" ]; tags = [ ]; releases_only = false; }`.
          - `reporter_pull_request` is taking
            `{ branches = [ ]; actions = [ "opened" "synchronize" "reopened" ]; require_approval = true; }`.
          - `time` is taking `{ cron = "0 0 2 * * *"; }`, six fields in UTC
            (`sec min hour dom mon dow`).

          Empty `branches`, `tags` and `actions` lists are matching everything.

          `require_approval` is defaulting to `true` for pull request triggers. It is parking
          evaluations of pull requests from contributors without write access on the Git host. A
          maintainer is releasing them with "Approve and run" on the GitHub check or a
          `/gradient approve` or `/gradient run` comment. `false` is running every pull request
          build automatically.
        '';
      };

      active = mkOption {
        type = types.bool;
        default = true;
        description = "Whether the trigger is active. Gradient is keeping inactive triggers but never firing them.";
      };
    };
  });

  actionType = types.submodule {
    options = {
      name = mkOption {
        type = types.str;
        description = ''
          Action name, unique within the task. Renaming it is creating a new action and deleting the
          old one on the next state apply.
        '';
      };

      type = mkOption {
        type = types.enum [
          "send_mail"
          "send_web_request"
          "git_host_status_report"
          "open_pr"
          "send_matrix_message"
          "send_slack_message"
        ];
        description = "Action kind, determining the expected `config`.";
      };

      active = mkOption {
        type = types.bool;
        default = true;
        description = "Whether the action is active. Gradient is keeping inactive actions but never firing them.";
      };

      events = mkOption {
        type = types.listOf types.str;
        default = [];
        description = ''
          Events the action is subscribing to. It must be empty for `git_host_status_report`, whose
          events are derived from build state.
        '';
      };

      config = mkOption {
        type = types.attrs;
        example = literalExpression ''
          { recipients = [ "ops@example.com" ]; }
        '';
        description = ''
          Type-specific configuration. Its shape is depending on `type`.

          - `send_mail` is taking
            `{ recipients = [ "ops@example.com" ]; subject_template = null; }`.
          - `send_web_request` is taking
            `{ url = "https://hooks.example.com/gradient"; token_file = "/etc/gradient/secrets/<name>-token"; }`.
          - `send_matrix_message` is taking
            `{ homeserver = "https://matrix.example.org"; room_id = "!abc:example.org"; access_token_file = "/etc/gradient/secrets/<name>-matrix"; }`.
            The room must be unencrypted and joined by the token's user.
          - `send_slack_message` is taking
            `{ webhook_url_file = "/etc/gradient/secrets/<name>-slack"; }`, the file holding the
            incoming webhook URL.
          - `git_host_status_report` is taking `{ integration = "gitea-prod"; }`, naming an
            outbound integration in the same project.
          - `open_pr` is opening a pull request on the Git host with the result of a generator.
            Its fields are listed below.
            - `integration` (string) is naming an outbound integration in the same project, as
              for `git_host_status_report`.
            - `generator` (string, default `"flake_lock"`) is choosing the change generator.
              `flake_lock` is currently the only one and is updating {file}`flake.lock`.
            - `granularity` (string, default `"per_run"`) is `"per_run"` for one pull request
              with every input update or `"per_input"` for one per updated input.
            - `verify_gate` (string, default `"build"`) is `"none"`, `"eval"` or `"build"`. The
              pull request is only opened once the generated change is passing that stage.
            - `branch_pattern` (string, default `"gradient/flake-lock-update"`) is the branch the
              pull request is opened from. It must contain the `{input}` placeholder for
              `per_input`, substituted with each input name.
            - `title_template` (string, optional) is the template of the pull request title.
            - `body_template` (string, optional) is the template of the pull request body.
            - `update_existing` (bool, default `true`) is force-pushing new contents to an open
              pull request of the same branch instead of opening a duplicate.

          A `send_web_request` action without `token_file` is sending unauthenticated requests.
          Secret files (`token_file`, `access_token_file`, `webhook_url_file`) are read from the
          systemd credential `gradient_action_''${name}_<field>` and stored encrypted with the
          server's crypt key.
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
          User name to grant membership to. A membership of a user not existing yet is applied on
          the user's registration or first OIDC sign-in.
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

  teamMemberType = types.submodule {
    options = {
      user = mkOption {
        type = types.str;
        description = "User name, resolved when the state is applied.";
      };
      role = mkOption {
        type = types.enum [ "Admin" "Member" ];
        default = "Member";
        description = "Role in the team. Admins manage members, workers, grants and requests.";
      };
    };
  };

  teamType = types.submodule ({ name, ... }: {
    options = {
      name = mkOption {
        type = types.str;
        default = name;
        defaultText = "<attrset key>";
        description = "Team name. Teams managed here cannot be changed through the API.";
      };
      display_name = mkOption {
        type = types.str;
        default = name;
        defaultText = "<attrset key>";
        description = "Display name of the team.";
      };
      members = mkOption {
        type = types.listOf teamMemberType;
        default = [ ];
        description = ''
          Users in the team. The next state apply removes members not listed, except members
          added through an OIDC or SCIM group.
        '';
      };
      oidc_group = mkOption {
        type = types.nullOr types.str;
        default = null;
        example = "platform-team";
        description = ''
          OIDC group whose members join the team on sign-in and leave it once the group is gone
          from their `groups` claim. The `groups` scope is required.
        '';
      };
      scim_group = mkOption {
        type = types.nullOr types.str;
        default = null;
        example = "acme-eng";
        description = "SCIM group mapped onto the team's members.";
      };
      new_projects = {
        users = mkOption {
          type = types.bool;
          default = false;
          description = "Whether every new project grants this team's users `new_projects.role`.";
        };
        workers = mkOption {
          type = types.bool;
          default = false;
          description = "Whether every new project grants this team's workers.";
        };
        role = mkOption {
          type = types.nullOr (types.enum [ "Admin" "Write" "View" ]);
          default = null;
          description = "Project role for the team's users on new projects.";
        };
      };
    };
  });

  projectTeamType = types.submodule {
    options = {
      team = mkOption {
        type = types.str;
        description = "Team granted on the project.";
      };
      role = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = ''
          Role of the team's users: a built-in `Admin`, `Write` or `View`, or a custom role of the
          project. Required when `users` is true.
        '';
      };
      users = mkOption {
        type = types.bool;
        default = true;
        description = "Whether the team's users get `role` on the project.";
      };
      workers = mkOption {
        type = types.bool;
        default = true;
        description = "Whether the team's workers take the project's jobs.";
      };
    };
  };

  cacheTeamType = types.submodule {
    options = {
      team = mkOption {
        type = types.str;
        description = "Team granted on the cache.";
      };
      role = mkOption {
        type = types.str;
        description = "Role of the team's users: `Admin`, `Write`, `View` or a custom role of the cache.";
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
          `manageCacheSettings`, `manageCacheKeys`, `manageUpstreamCaches`, `manageCacheMembers`,
          `manageCacheRoles`, `manageCacheSubscriptions`, `manageCacheWebhooks` or `deleteCache`.
        '';
      };
    };
  };

  cacheType = types.submodule ({ config, name, ... }: {
    imports = [ (stateAlias "upstreams" "upstream_caches") ];

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
        description = "Description of the cache. `null` is leaving it empty.";
      };

      active = mkOption {
        type = types.bool;
        default = true;
        description = "Whether the cache is active. The UI can toggle this value, and the next server start is restoring it.";
      };

      priority = mkOption {
        type = types.ints.positive;
        default = 10;
        description = ''
          Priority advertised in {file}`nix-cache-info`. Nix is querying caches with a lower value
          first.
        '';
      };

      local_priority = mkOption {
        type = types.nullOr types.int;
        default = null;
        description = ''
          Priority advertised in {file}`nix-cache-info` to clients within
          {option}`services.gradient.http.localIps`. `null` or `0` is disabling the override.
        '';
      };

      max_storage_gb = mkOption {
        type = types.ints.unsigned;
        default = 0;
        description = ''
          Storage limit of the cache in GB. New evaluations are waiting while every writable cache
          of a project has less than 10 MiB left. `0` is disabling the limit.
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

      upstream_caches = mkOption {
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

      teams = mkOption {
        type = types.listOf cacheTeamType;
        default = [ ];
        description = ''
          Teams granted on this cache. An empty list leaves the cache's grants alone; a non-empty
          list is the source of truth and the next state apply removes grants not listed.
        '';
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
          Worker identity. It must match {option}`services.gradient.worker.id` on the worker host.
        '';
      };

      url = mkOption {
        type = types.nullOr types.str;
        default = null;
        example = "wss://worker.example.com/proto";
        description = ''
          WebSocket URL on which the worker is accepting server connections. The server is
          connecting to the worker with a URL set. `null` is letting the worker connect to the
          server.
        '';
      };

      projects = mkOption {
        type = types.listOf types.str;
        default = [ ];
        example = [ "acme-corp" "globex" ];
        description = ''
          Projects the worker is registered under, one registration per project. Leave empty for a
          team worker.
        '';
      };

      team = mkOption {
        type = types.nullOr types.str;
        default = null;
        example = "platform";
        description = ''
          Team owning this worker. A team worker serves every project granted the team's workers
          and is mutually exclusive with `projects`.
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
          User name of the registration's creator. `null` is leaving it unattributed, as for a
          worker a host is provisioning for itself.
        '';
      };

      enable_fetch = mkOption {
        type = types.bool;
        default = true;
        description = ''
          Whether the server is granting this registration the worker's `fetch` capability.
        '';
      };

      enable_eval = mkOption {
        type = types.bool;
        default = true;
        description = "Whether the server is granting this registration the worker's `eval` capability.";
      };

      enable_build = mkOption {
        type = types.bool;
        default = true;
        description = ''
          Whether the server is granting this registration the worker's `build` capability.
        '';
      };

      enabled = mkOption {
        type = types.bool;
        default = true;
        description = "Whether the worker is active. The next server start restores it.";
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
          prefix. The server is only storing and comparing hashes. Generate it with
          {command}`printf %s "$TOKEN" | sha256sum | cut -d' ' -f1`.
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
          `triggerEvaluation`, `editTask` or `manageMembers`. It must not be empty.
          `GET /user/keys/permissions` is listing them all.
        '';
      };

      project = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = ''
          Project the key is restricted to. `null` is allowing every project the owner is a member
          of.
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
        description = "Project owning the role. Roles managed here are always project-scoped.";
      };

      permissions = mkOption {
        type = types.listOf types.str;
        example = [ "viewProject" "triggerEvaluation" ];
        description = ''
          Permissions granted by the role, as camelCase identifiers. It must not be empty.
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
          `--state-validate`. Schema and reference errors are then failing the Nix build instead of
          the first server start. No database is touched.
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
        description = "Users to create, one entry per user name.";
      };

      projects = mkOption {
        type = types.attrsOf projectType;
        default = { };
        description = "Projects to create, one entry per name.";
      };

      tasks = mkOption {
        type = types.attrsOf taskType;
        default = { };
        description = "Tasks to create, one entry per name.";
      };

      integrations = mkOption {
        type = types.attrsOf integrationType;
        default = { };
        example = literalExpression ''
          {
            acme-prod-inbound = {
              project = "acme-corp";
              kind = "inbound";
              git_host_type = "gitea";
              secret_file = "/etc/gradient/secrets/acme-inbound-hmac";
              created_by = "alice";
            };
            acme-status-reports = {
              project = "acme-corp";
              kind = "outbound";
              git_host_type = "gitea";
              endpoint_url = "https://gitea.example.com";
              access_token_file = "/etc/gradient/secrets/acme-gitea-token";
              created_by = "alice";
            };
            acme-github-out = {
              project = "acme-corp";
              kind = "outbound";
              git_host_type = "github";
              installation_id = 12345678;
              account_login = "acme-corp";
              created_by = "alice";
            };
          }
        '';
        description = ''
          Git host integrations per project, one entry per name. Secrets of inbound and tokens of
          outbound integrations are read as systemd credentials and stored encrypted.
        '';
      };

      caches = mkOption {
        type = types.attrsOf cacheType;
        default = { };
        description = "Caches to create, one entry per name.";
      };

      roles = mkOption {
        type = types.attrsOf roleType;
        default = { };
        description = ''
          Custom roles, one entry per role name. They cannot be modified or deleted through the API.
        '';
      };

      api_keys = mkOption {
        type = types.attrsOf apiKeyType;
        default = { };
        description = "API keys to create, one entry per name.";
      };

      teams = mkOption {
        type = types.attrsOf teamType;
        default = { };
        description = "Teams, one entry per team name.";
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
          Worker registrations, one entry per worker ID. The token from `token_file` is stored
          hashed and never persisted in plain text.
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

  config.warnings = concatMap (def:
    concatMap (r:
      let entries = def.value.${r.group} or { }; in
      mapAttrsToList (entry: _:
        "The option `services.gradient.state.${r.group}.${entry}.${r.from}' defined in ${def.file} has been renamed to `${r.to}'."
      ) (filterAttrs (_: v: isAttrs v && v ? ${r.from}) (if isAttrs entries then entries else { }))
    ) renamedStateOptions
  ) options.services.gradient.state.definitionsWithLocations;

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
        valid = !(a.type == "git_host_status_report" && a.events != []);
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
      git_host_status_report actions cannot declare custom `events`.
    '';
  }) invalidActions;
}
