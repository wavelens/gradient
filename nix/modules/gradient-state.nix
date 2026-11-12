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
          Name of the internal Gradient cache to use. Required for `internal` upstream caches.
        '';
      };

      display_name = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = ''
          Display name of the upstream cache. Required for `external` upstream caches. `null`
          uses the name of the internal cache.
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
        description = "URL of the external Nix binary cache. Required for `external` upstream caches.";
      };

      public_key = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = ''
          Public key of the external Nix binary cache. Required for `external` upstream caches.
        '';
      };

      http1_only = mkOption {
        type = types.bool;
        default = false;
        description = ''
          Whether requests to the external Nix binary cache use HTTP/1.1 only. Enable this
          for caches that break HTTP/2 transfers.
        '';
      };

      active = mkOption {
        type = types.bool;
        default = true;
        description = ''
          Whether the upstream cache is active. Gradient keeps inactive upstream caches but never
          queries them. The UI can toggle this value, and the next server start restores it.
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
          File containing the hashed password. `null` creates the account without a local
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
          Project UUID. `null` lets the server generate one. Set it to reference the project
          as `<id>:<token>` in a worker's {option}`services.gradient.worker.peersFile` in a fully
          declarative deployment. The server applies it only on creation and rejects a value
          conflicting with an existing project. Generate one with {command}`uuidgen`.
        '';
      };

      description = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = "Description of the project. Set to `null` to leave it empty.";
      };

      private_key_file = mkOption {
        type = types.str;
        description = "File containing the SSH private key used for Git access.";
      };

      secret_files = mkOption {
        type = types.attrsOf types.str;
        default = { };
        example = { wandb-token = "/run/secrets/wandb-token"; };
        description = ''
          Files with the project secrets of exec jobs, keyed by secret name. The server reads each
          file as a systemd credential and stores its content encrypted.
        '';
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
          UI. The task still receives evaluations from {command}`gradient build`.
        '';
      };

      allow_exec_in_build_requests = mkOption {
        type = types.bool;
        default = false;
        description = ''
          Build requests from {command}`gradient build` and {command}`gradient run` start the exec jobs that match them. The instance and worker switches still apply.
        '';
      };

      restrict_evaluation_fetches = mkOption {
        type = types.bool;
        default = false;
        description = ''
          Evaluate projects with Nix `restrict-eval`. Fetches during evaluation (`builtins.fetchTree`, `fetchurl`, `fetchTarball`, `fetchGit`) can access only the inputs locked in `flake.lock`, the server's allowed URL prefixes and `extra_allowed_uri_prefixes`. Set `false` to let projects fetch any URL during evaluation.
        '';
      };

      extra_allowed_uri_prefixes = mkOption {
        type = types.listOf types.str;
        default = [];
        example = [ "https://example.com/archives" ];
        description = ''
          URL prefixes this project can fetch during evaluation. They add to the URL prefixes that locked flake inputs and the server allow. Prefixes match the same URL or a URL under it at the `/` position. Entries like `github:` allow a whole URL scheme. Nothing changes when `restrict_evaluation_fetches` is `false`.
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
          Users with roles on this project. An empty list keeps the legacy behaviour, making
          `created_by` Admin and reconciling no other memberships.

          A non-empty list is the source of truth. The next state apply revokes memberships not
          listed and does not make `created_by` Admin implicitly. Members referring to users that do
          not exist yet are applied on the user's registration or first OIDC sign-in.
        '';
      };

      teams = mkOption {
        type = types.listOf projectTeamType;
        default = [ ];
        description = ''
          Teams granted on this project. The next state apply removes grants declared here
          before and no longer listed. Grants from the API or from new-project settings stay.
        '';
      };
      owner_team = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = "The team with ownership of the project, or `null` when no owner. The team's `new_projects` settings decide the grant for that team on the project.";
      };
    };
  });

  flakeInputOverrideType = types.submodule ({ ... }: {
    options = {
      url = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = ''
          Flake reference overriding this input. `null` together with `keep_url` force-updates
          the input from the URL declared in the task's {file}`flake.nix`.
        '';
      };
      keep_url = mkOption {
        type = types.bool;
        default = false;
        description = ''
          Whether to force-update this input from its flake-declared URL. Mutually exclusive with
          `url`; set exactly one of the two.
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
        description = "Description of the task. Set to `null` to leave it empty.";
      };

      repository = mkOption {
        type = types.str;
        description = "Git repository URL of the task.";
      };

      wildcard = mkOption {
        type = types.str;
        default = "packages.x86_64-linux.**";
        description = ''
          Comma-separated Nix attribute paths to evaluate from the flake. A `*` segment takes any
          attribute name, `**` also takes a nested set below it, and `x86_64-*` matches part of a
          name. A pattern prefixed with `!` excludes matching paths.
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
          Retain this number of finished evaluations for metrics and history. Active evaluations
          never block garbage collection and stay outside this count. Always keep the most recent
          finished evaluation, whatever its result. The remaining slots go first to the most recent
          completed evaluations, then to the most recent failed or aborted evaluations. Set this to
          at least 1 and at most {option}`services.gradient.eval.maxKeep`.
        '';
      };

      wait_for_workers = mkOption {
        type = types.bool;
        default = false;
        description = ''
          Whether an evaluation waits for a worker when its builds need an architecture or
          system features no connected worker provides. Otherwise the server aborts such an
          evaluation with a warning naming what is missing.
        '';
      };

      retry_failed_builds = mkOption {
        type = types.bool;
        default = true;
        description = ''
          Whether a new evaluation of this task queues its permanently failed builds and their
          dependents again. Otherwise they stay failed until a user retries the build or the
          evaluation.
        '';
      };

      multi_node_evaluation = mkOption {
        type = types.bool;
        default = false;
        description = ''
          Whether to let more than one eval worker run an evaluation of this task. The server
          splits an evaluation only when past evaluations of the task predict a gain.
        '';
      };

      exec_enabled = mkOption {
        type = types.bool;
        default = false;
        description = "Whether to run exec jobs of this task. Exec entry points of a task with this option off fail with a message.";
      };

      only_evaluate = mkOption {
        type = types.bool;
        default = false;
        description = "Whether the evaluation of this task can run an evaluation alone. Such evaluations end at evaluation completion, build nothing, and run no exec jobs.";
      };

      concurrency = mkOption {
        type = types.enum [ "auto" "hard_abort" "soft_abort" "skip" "all" ];
        default = "auto";
        description = ''
          Behavior of a new trigger event while an evaluation of the same branch runs.
          Evaluations of other branches keep running.

          - `auto` starts the new evaluation alongside the running one.
            Once the new evaluation starts building, the older evaluations are cancelled like
            with `hard_abort`, apart from builds the new evaluation also needs.
            A failed evaluation of the new commit leaves the running evaluation alone.
          - `hard_abort` cancels the running evaluation and its builds and starts a new one.
          - `soft_abort` marks the running evaluation aborted and makes the new one canonical.
            Its builds finish, and their outputs flow into the new evaluation.
          - `skip` discards the new event.
          - `all` runs the new evaluation alongside the current one.
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
          `null` leaves existing triggers untouched, and a new task declared with `null`
          starts with a polling trigger as its default trigger. An empty list is rejected.
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
          Overrides applied when fetching flake inputs, one entry per input name. An empty set
          uses {file}`flake.lock` as is.
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
              name = "flake-lock-pr";
              type = "open_pr";
              config = {
                git_host_connection = "gitea-prod";
                generator = "flake_lock";
                granularity = "per_input";
                verify_gate = "build";
                branch_pattern = "gradient/flake-lock-update/{input}";
                title_template = "flake.lock: update {input}";
                body_template = "Automated flake input update opened by Gradient.";
                update_existing = true;
              };
            }
            {
              name = "evaluate-deploy";
              type = "start_evaluation";
              events = [ "exec_job.completed" ];
              config.task = "deploy";
            }
          ]
        '';
        description = ''
          Task actions include email notifications, web requests, Matrix and Slack messages, pull request automation and evaluation of other tasks. Actions that don't exist at the next state application receive removal. `name` can match those actions.

          Keep the secret files of an action (`token_file`, `access_token_file`, `webhook_url_file`) at the path for systemd credentials, `''${GRADIENT_CREDENTIALS_DIR}/gradient_action_<project>_<task>_<action>_<field>`.
        '';
      };

      git_host_connection = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = ''
          The name of the connection to the Git host for the task. Webhooks for push and pull requests arrive at this connection. This connection can report commit statuses. Gradient can search for that name in the task project, then in the team owning the project, then in the instance. Triggers `reporter_push` and `reporter_pull_request` need a connection with `receives_events = true`. Connections for a GitHub App also count. Declare the connection in `git_host_connections`. Use `null` to leave the task without a connection.
        '';
      };

      report_status = mkOption {
        type = types.bool;
        default = false;
        description = ''
          Gradient can report the commit status for each evaluation to the Git host. A `git_host_connection` with `reports_status = true` is necessary.
        '';
      };

      created_by = mkOption {
        type = types.str;
        description = "User name of the task's creator.";
      };
    };
  });

  gitHostConnectionType = types.submodule ({ config, name, ... }: {
    options = {
      name = mkOption {
        type = types.str;
        default = name;
        defaultText = "<attrset key>";
        description = "Connection name. Unique for each owner.";
      };

      display_name = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = "Display name for the connection. Set the value to `null` to use `name`.";
      };

      owner = mkOption {
        type = types.str;
        example = "team/acme";
        description = ''
          Connection owner: `instance`, `team/<name>` or `project/<name>`. Tasks use a connection from their project, from the team for that project, or from the instance. Project connections take precedence over team connections with the same name. Team connections take precedence over instance connections with the same name.
        '';
      };

      git_host_type = mkOption {
        type = types.enum [ "gitea" "forgejo" "gitlab" "github" ];
        description = ''
          Git host of the connection. `owner = "instance"` and `installation_id` replace a secret, token and base URL for `github`. Installations of the GitHub App create such a connection automatically. Declared connections with the same installation merge into that connection.
        '';
      };

      receives_events = mkOption {
        type = types.bool;
        default = true;
        description = ''
          Whether the connection can receive push and pull request webhooks from the Git host `/api/v1/hooks/<connection-id>`.
        '';
      };

      reports_status = mkOption {
        type = types.bool;
        default = false;
        description = ''
          Whether a task with `report_status` can report commit status through a connection. Set `base_url` and `access_token_file` together with that option on hosts other than GitHub.
        '';
      };

      installation_id = mkOption {
        type = types.nullOr types.int;
        default = null;
        description = ''
          The installation ID of a GitHub App. This value is the number at the end of the installation URL. Set this value for `git_host_type = "github"` connections. Other Git hosts reject this value.
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
          A file with the HMAC signing secret for webhooks from the Git host. Gradient loads this file as a systemd credential and stores the secret in encrypted form.
        '';
      };

      base_url = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = ''
          Base URL of the Git host API, such as `https://gitea.example.com`, for commit status reports and pull requests.
        '';
      };

      access_token_file = mkOption {
        type = types.nullOr types.path;
        default = null;
        description = ''
          File for a Git host API token. Commit status reports and pull requests use this token. Gradient can load this file as a systemd credential and save the file in encrypted form. GitHub connections get their credentials from {option}`services.gradient.githubApp` instead.
        '';
      };

      created_by = mkOption {
        type = types.str;
        description = "The username of the user who created the connection.";
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

      config = mkOption {
        type = types.attrs;
        default = { };
        example = literalExpression ''
          { interval_secs = 60; }
        '';
        description = ''
          Type-specific configuration. Its shape depends on `type`.

          - `polling` takes `{ interval_secs = 300; branch = "main"; }`. The interval is at
            least 10 seconds, and `branch` defaults to the remote HEAD.
          - `reporter_push` takes
            `{ branches = [ "main" "release/*" ]; tags = [ ]; releases_only = false; include_flake_inputs = false; }`.
            The task's Git host connection can receive a push report for a repo of a tracked flake input. That report can also make `include_flake_inputs = true` start an update of the task's flake inputs. Use
            the branch for that input as the push target, or the default branch of the repository
            for an input with no branch.
          - `reporter_pull_request` takes
            `{ branches = [ ]; actions = [ "opened" "synchronize" "reopened" ]; require_approval = true; }`.
          - `time` can receive `{ cron = "0 2 * * *"; branch = "main"; }` in UTC. There are 5
            fields in `cron` (`min hour dom mon dow`, seconds are `0`) or 6 fields
            (`sec min hour dom mon dow`). The default for `branch` is remote HEAD.

          Empty `branches`, `tags` and `actions` lists match everything.

          `require_approval` defaults to `true` for pull request triggers. It parks
          evaluations of pull requests from contributors without write access on the Git host. A
          maintainer releases them with "Approve and run" on the GitHub check or a
          `/gradient approve` or `/gradient run` comment. `false` runs every pull request
          build automatically.
        '';
      };

      active = mkOption {
        type = types.bool;
        default = true;
        description = "Whether the trigger is active. Gradient keeps inactive triggers but never fires them automatically.";
      };

      is_default = mkOption {
        type = types.bool;
        default = false;
        description = ''
          Whether the trigger is the default trigger of the task. The "Start Evaluation" button
          and the evaluate API fire the default trigger. At most one trigger of a task sets this
          flag, and the first trigger is the default when none does.
        '';
      };
    };
  });

  actionType = types.submodule {
    options = {
      name = mkOption {
        type = types.str;
        description = ''
          Action name, unique within the task. Renaming it creates a new action and deletes the
          old one on the next state apply.
        '';
      };

      type = mkOption {
        type = types.enum [
          "send_mail"
          "send_web_request"
          "open_pr"
          "send_matrix_message"
          "send_slack_message"
          "start_evaluation"
        ];
        description = "Action kind, determining the expected `config`.";
      };

      active = mkOption {
        type = types.bool;
        default = true;
        description = "Whether the action is active. Gradient keeps inactive actions but never fires them.";
      };

      events = mkOption {
        type = types.listOf types.str;
        default = [];
        description = "Events that the action can subscribe to.";
      };

      config = mkOption {
        type = types.attrs;
        example = literalExpression ''
          { recipients = [ "ops@example.com" ]; }
        '';
        description = ''
          Type-specific configuration. Its shape depends on `type`.

          - `send_mail` takes
            `{ recipients = [ "ops@example.com" ]; subject_template = null; }`.
          - `send_web_request` takes
            `{ url = "https://hooks.example.com/gradient"; token_file = "/etc/gradient/secrets/<name>-token"; }`.
          - `send_matrix_message` takes
            `{ homeserver = "https://matrix.example.org"; room_id = "!abc:example.org"; access_token_file = "/etc/gradient/secrets/<name>-matrix"; }`.
            The room must be unencrypted and joined by the token's user.
          - `send_slack_message` takes
            `{ webhook_url_file = "/etc/gradient/secrets/<name>-slack"; }`, the file holding the
            incoming webhook URL.
          - `open_pr` opens a pull request on the Git host with the result of a generator.
            It takes the following fields.
            - `git_host_connection` (string) names the Git host connection that opens the pull request. Gradient looks it up like the task's `git_host_connection`.
            - `generator` (string, default `"flake_lock"`) chooses the change generator.
              `flake_lock` updates {file}`flake.lock`. `exec_tree` opens the
              {file}`gradient/tree.patch` output of an exec job once the job completes, with
              `{entry_point}` and `{job}` as placeholders in the branch and templates.
            - `granularity` (string, default `"per_run"`) is `"per_run"` for one pull request
              with every input update or `"per_input"` for one per updated input.
            - `verify_gate` (string, default `"build"`) is `"none"`, `"eval"` or `"build"`. The
              action opens the pull request only once the generated change passes that stage.
            - `branch_pattern` (string, default `"gradient/flake-lock-update"`) is the branch the
              pull request is opened from. It must contain the `{input}` placeholder for
              `per_input`, substituted with each input name.
            - `title_template` (string, optional) is the template of the pull request title.
            - `body_template` (string, optional) is the template of the pull request body.
            - `update_existing` (bool, default `true`) force-pushes new contents to an open
              pull request of the same branch instead of opening a duplicate.

          - `start_evaluation` takes `{ task = "deploy"; }`, naming a task of the same
            project and starts an evaluation of that task's head commit on each subscribed
            event. An exec job event links the new evaluation to the job.

          A `send_web_request` action without `token_file` sends unauthenticated requests.
          The secret files (`token_file`, `access_token_file`, `webhook_url_file`) come from systemd credentials `gradient_action_<project>_<task>_<action>_<field>`. The server crypt key can encrypt them for storage.
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
          User name to grant membership to. A membership of a user that does not exist yet is applied
          on the user's registration or first OIDC sign-in.
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
        description = ''
          Team name. The settings of a team managed here cannot be changed through the API, while
          other members and workers can still be added there.
        '';
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
          Users in the team. The next state apply removes members the state declared before and no
          longer lists. Members added through the API,
          {option}`services.gradient.state.teams.<name>.oidc_group` or
          {option}`services.gradient.state.teams.<name>.scim_group` stay.
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
          description = "Whether projects that belong to this team grant `new_projects.role` to users on this team. The UI label is \"Default role of members on team projects\".";
        };
        workers = mkOption {
          type = types.bool;
          default = false;
          description = "Whether new projects get the workers of this team, regardless of the project owner.";
        };
        role = mkOption {
          type = types.nullOr (types.enum [ "Admin" "Write" "View" ]);
          default = null;
          description = "Project role for users of the team in projects that belong to the team.";
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

  cacheProjectType = types.submodule {
    options = {
      name = mkOption {
        type = types.str;
        description = "The name of a project with a subscription to this cache.";
      };
      mode = mkOption {
        type = types.enum [ "ReadWrite" "ReadOnly" "WriteOnly" ];
        default = "ReadWrite";
        description = "Subscriptions have an access mode. A project can pull and push in ReadWrite mode. ReadOnly mode: pull only. WriteOnly mode: push only.";
      };
      sign_outputs = mkOption {
        type = types.bool;
        default = true;
        description = ''
          Projects can push outputs to a cache. This option can set whether those outputs carry a signature with the cache key. Nix clients outside the cache don't trust unsigned paths, so those outputs stay private even in a public cache. Another signing subscription can push a path as well and those paths still get a signature.
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
          `manageCacheSettings`, `manageCacheKeys`, `manageUpstreamCaches`, `manageCacheMembers`,
          `manageCacheRoles`, `manageCacheSubscriptions`, `manageCacheWebhooks` or `deleteCache`.
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
        description = "Description of the cache. Set to `null` to leave it empty.";
      };

      active = mkOption {
        type = types.bool;
        default = true;
        description = "Whether the cache is active. The UI can toggle this value, and the next server start restores it.";
      };

      priority = mkOption {
        type = types.ints.positive;
        default = 10;
        description = ''
          Priority advertised in {file}`nix-cache-info`. Nix queries caches with a lower value
          first.
        '';
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
          Storage limit of the cache in GB. New evaluations wait while every writable cache
          of a project has less than 10 MiB left. `0` disables the limit.
        '';
      };

      pull_through = mkOption {
        type = types.bool;
        default = true;
        description = ''
          Whether Nix clients receive paths from the upstream caches through this cache. Caches
          without pull-through only deliver the paths they hold. Builds still substitute from
          the upstream caches.
        '';
      };

      signing_key_file = mkOption {
        type = types.str;
        description = "File containing the Nix cache signing key.";
      };

      projects = mkOption {
        type = types.listOf (types.coercedTo types.str (name: { inherit name; }) cacheProjectType);
        default = [ ];
        description = "Projects with a subscription to this cache. Names that reference only a project subscribe with ReadWrite and signed outputs. The server can lock these subscriptions. The web UI and API can't change or delete them.";
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
          Teams granted on this cache. The next state apply removes grants declared here before
          and no longer listed. Grants from the API stay.
        '';
      };

      owner_team = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = "The team with ownership of the cache. If `null`, the cache can have no owner team.";
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
          WebSocket URL on which the worker accepts server connections. With a URL set, the
          server connects to the worker. `null` lets the worker connect to the server.
        '';
      };

      projects = mkOption {
        type = types.listOf types.str;
        default = [ ];
        example = [ "acme-corp" "globex" ];
        description = ''
          Projects the worker is registered under, one registration per project. Leave it empty for a
          team worker.
        '';
      };

      team = mkOption {
        type = types.nullOr types.str;
        default = null;
        example = "platform";
        description = ''
          Team owning this worker. A team worker can build for every project granting the team's
          workers. Mutually exclusive with `projects`.
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
          User name of the registration's creator. `null` leaves it unattributed, as for a
          worker a host provisions for itself.
        '';
      };

      enable_network = mkOption {
        type = types.bool;
        default = true;
        description = "Whether to grant this registration the worker's `network` capability.";
      };

      enable_secrets = mkOption {
        type = types.bool;
        default = true;
        description = "Whether to grant this registration the worker's `secrets` capability.";
      };

      enable_exec = mkOption {
        type = types.bool;
        default = false;
        description = "Whether to grant this registration the worker's `exec` capability.";
      };

      enable_eval = mkOption {
        type = types.bool;
        default = true;
        description = "Whether to grant this registration the worker's `eval` capability.";
      };

      enable_build = mkOption {
        type = types.bool;
        default = true;
        description = ''
          Whether to grant this registration the worker's `build` capability.
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
          prefix. The server only stores and compares hashes. Generate it with
          {command}`printf %s "$TOKEN" | sha256sum | cut -d' ' -f1`.
        '';
      };

      owned_by = mkOption {
        type = types.str;
        description = "User name of the key's owner.";
      };

      permissions = mkOption {
        type = types.listOf types.str;
        default = [ ];
        example = [ "viewProject" "triggerEvaluation" ];
        description = ''
          The key can grant permissions as camelCase identifiers such as `viewProject`, `triggerEvaluation`, `editTask`, or `manageMembers`. Keep at least one of `permissions` and `cache_permissions` non-empty. All of them appear in `GET /user/keys/permissions`. Permission sets in Gradient always include `viewProject`. The key can create projects through `createProject`, if `project = null` is present and not otherwise. The key can use the owner's superuser rights through `useSuperuserRights` alone. Both `project = null` on the key and `superuser = true` on the owner are necessary for that use.
        '';
      };

      cache_permissions = mkOption {
        type = types.listOf types.str;
        default = [ ];
        example = [ "readStore" ];
        description = ''
          A key can receive cache permissions `viewCache`, `readStore`, `writeStore`, `manageCacheSettings`, `manageCacheKeys`, `manageUpstreamCaches`, `manageCacheMembers`, `manageCacheRoles`, `manageCacheSubscriptions`, `manageCacheWebhooks` or `deleteCache`. The key can get these permissions on every cache where the owner of the key has a role. The role of the owner in that cache can limit those permissions. Only keys with `project = null` can have cache permissions.
        '';
      };

      project = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = ''
          Project the key is restricted to. `null` allows every project the owner is a member
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
          Whether to check the state option with the server's `--state-validate` flag at
          build time. Schema and reference errors then fail at build time instead of at
          runtime.
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

      git_host_connections = mkOption {
        type = types.attrsOf gitHostConnectionType;
        default = { };
        example = literalExpression ''
          {
            gitea-prod = {
              owner = "team/acme";
              git_host_type = "gitea";
              secret_file = "/etc/gradient/secrets/acme-gitea-hmac";
              base_url = "https://gitea.example.com";
              access_token_file = "/etc/gradient/secrets/acme-gitea-token";
              reports_status = true;
              created_by = "alice";
            };
            acme-gitlab = {
              owner = "project/acme-corp";
              git_host_type = "gitlab";
              secret_file = "/etc/gradient/secrets/acme-gitlab-hmac";
              created_by = "alice";
            };
            github-acme-corp = {
              owner = "instance";
              git_host_type = "github";
              installation_id = 12345678;
              account_login = "acme-corp";
              reports_status = true;
              created_by = "alice";
            };
          }
        '';
        description = ''
          Git host connections. One entry per label. Gradient can read the secret and token from systemd credentials `gradient_git_host_connection_<label>_secret` and `gradient_git_host_connection_<label>_token`, then encrypt and store both.
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
              wildcard = "nixosConfigurations.**.config.system.build.toplevel";
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
      description = ''
        Declarative Gradient state, written as JSON to the server's state file.
        The [state reference](https://wavelens.github.io/gradient/reference/state) documents the available fields.
        Fields naming an entry, such as `name`, `username` and `display_name`, default to its attribute name.
        Files in `password_file`, `private_key_file`, `signing_key_file`, `key_file`,
        `token_file`, `secret_file`, `access_token_file` and the secret files of task
        actions become systemd credentials of the server.
      '';
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
  in map (b: {
    assertion = false;
    message = ''
      services.gradient.state.tasks.${b.task}.flake_input_overrides.${b.input}: \
      Set either `url` (string) or `keep_url = true`, not both.
    '';
  }) invalid;
}
