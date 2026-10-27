/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ lib, pkgs, config, options, ... }: let
  cfg = config.services.gradient.worker;
  exec = cfg.exec;

  execByHand = [
    [ "exec" "image" ]
    [ "exec" "devices" ]
    [ "exec" "mounts" ]
    [ "exec" "allow" "ports" ]
    [ "exec" "cloudHypervisorBin" ]
    [ "exec" "virtiofsdBin" ]
  ];

  subnetParts = lib.splitString "/" exec.subnet;
  subnetOctets = lib.splitString "." (lib.head subnetParts);
  bridgeAddress = lib.concatStringsSep "." (lib.take 3 subnetOctets ++ [ (toString (lib.toInt (lib.last subnetOctets) + 1)) ]);
  execOptionsSet = lib.any (option: option.highestPrio < (lib.mkOptionDefault null).priority)
    (lib.collect lib.isOption options.services.gradient.worker.exec);
  guestDnsForwarder = exec.allow.network && exec.nameservers == [ ];
  hasDevice = source: lib.any (device: device.${source} != null && device.${source} != [ ]) exec.devices;

  execConfig = lib.mkIf cfg.capabilities.exec {
    security.polkit = {
      enable = true;
      extraConfig = ''
        polkit.addRule(function(action, subject) {
          if (action.id == "org.freedesktop.systemd1.manage-units" &&
              subject.user == "gradient-worker" &&
              (action.lookup("unit") || "").indexOf("gradient-guest-") == 0) {
            return polkit.Result.YES;
          }
        });
      '';
    };

    users.users.gradient-worker.extraGroups = [ "kvm" ];

    systemd.tmpfiles.rules = map (mount: "d ${mount.path} 0770 gradient-worker gradient-worker -")
      (lib.filter (mount: mount.perJob) exec.mounts);

    services.udev.extraRules = lib.mkIf (hasDevice "vfio") ''
      SUBSYSTEM=="vfio", GROUP="gradient-worker", MODE="0660"
    '';

    boot.kernelModules = lib.mkIf (hasDevice "usb") [ "usbip-host" ];

    systemd.services.usbipd = lib.mkIf (hasDevice "usb") {
      description = "USB/IP server for Gradient exec job devices";
      wantedBy = [ "multi-user.target" ];
      after = [ "network.target" ];
      serviceConfig = {
        ExecStart = "${lib.getExe' cfg.packages.usbip "usbipd"} -4";
        IPAddressAllow = "localhost";
        IPAddressDeny = "any";
      };
    };

    systemd.services.gradient-guest-dns = lib.mkIf guestDnsForwarder {
      description = "Forwarder for DNS queries from Gradient guests.";
      wantedBy = [ "multi-user.target" ];
      after = [ "network.target" ];
      serviceConfig = {
        ExecStart = lib.concatStringsSep " " [
          (lib.getExe' pkgs.dnsmasq "dnsmasq")
          "--keep-in-foreground"
          "--conf-file=/dev/null"
          "--pid-file"
          "--no-hosts"
          "--bind-dynamic"
          "--interface=${exec.bridge}"
          "--except-interface=lo"
        ];
        Restart = "on-failure";
      };
    };

    networking = {
      bridges.${exec.bridge}.interfaces = [ ];
      interfaces.${exec.bridge}.ipv4.addresses = [{
        address = bridgeAddress;
        prefixLength = lib.toInt (lib.last subnetParts);
      }];
      nat = lib.mkIf exec.allow.network {
        enable = true;
        internalInterfaces = [ exec.bridge ];
      };
      firewall.interfaces.${exec.bridge} = lib.mkIf guestDnsForwarder {
        allowedUDPPorts = [ 53 ];
        allowedTCPPorts = [ 53 ];
      };
    };
  };

  workerOptions = import ./options.nix {
    inherit lib;
    specs = builtins.fromJSON (builtins.readFile ../../backend/gradient-worker/options.json);
    subGroups = { eval = [ "cache" ]; log = [ "level" ]; exec = [ "allow" "defaults" ]; };
    groupSwitches.exec = [ "capabilities" "exec" ];
    emptyListIsUnset = true;

    examples = {
      endpoint = "10.0.0.7:7000";
      id = "550e8400-e29b-41d4-a716-446655440001";
      serverUrl = "wss://gradient.example.com/proto";
      zone = "fra1";
      log.traceDir = "/var/lib/gradient-worker/trace";
    };

    withoutEnvironment = [ [ "peers" ] [ "sshBin" ] [ "evalSubprocess" ] [ "evalDriver" ] [ "guestSupervisor" ] ] ++ execByHand;

    writtenByHand = execByHand ++ [
      [ "peers" ]
      [ "sshBin" ]
      [ "evalSubprocess" ]
      [ "evalDriver" ]
      [ "guestSupervisor" ]
      [ "system" "architectures" ]
      [ "system" "features" ]
      [ "nixDaemon" "maxConnections" ]
      [ "build" "cgroup" ]
      [ "nar" "chunkBytes" ]
      [ "log" "level" "default" ]
      [ "log" "level" "eval" ]
      [ "log" "level" "build" ]
      [ "log" "level" "proto" ]
    ];
  };

  logLevelType = lib.types.enum [ "trace" "debug" "info" "warn" "error" ];
  environmentValue = value: if lib.isBool value then lib.boolToString value else lib.mapNullable toString value;
in {
  disabledModules = [ "services/continuous-integration/gradient/worker.nix" ];

  imports = [
    { options.services.gradient.worker = workerOptions.options; }
  ];

  options.services.gradient.worker = {
    enable = lib.mkEnableOption "the Gradient worker";

    environmentVariables = lib.mkOption {
      type = with lib.types; lazyAttrsOf (nullOr (oneOf [ bool int str path ]));
      default = { };
      example = { GRADIENT_WORKER_BUILD_MAX_CONCURRENT = 8; };
      description = ''
        Environment variables of the Gradient worker.
        The [configuration reference](https://wavelens.github.io/gradient/reference/configuration) lists every variable and its default.
        Set a variable to `null` to unset it.
      '';
    };

    packages = {
      gradient = lib.mkPackageOption pkgs "gradient" { };
      git = lib.mkOption {
        type = lib.types.package;
        default = config.programs.git.package;
        defaultText = lib.literalExpression "config.programs.git.package";
        description = "Git package available to the worker for cloning repositories.";
      };

      ssh = lib.mkOption {
        type = lib.types.package;
        default = config.programs.ssh.package;
        defaultText = lib.literalExpression "config.programs.ssh.package";
        description = ''
          OpenSSH package used as {env}`GIT_SSH_COMMAND` to fetch private flake inputs.
        '';
      };

      cloudHypervisor = lib.mkPackageOption pkgs "cloud-hypervisor" { };
      virtiofsd = lib.mkPackageOption pkgs "virtiofsd" { };

      usbip = lib.mkOption {
        type = lib.types.package;
        default = config.boot.kernelPackages.usbip;
        defaultText = lib.literalExpression "config.boot.kernelPackages.usbip";
        description = ''
          USB/IP package with {command}`usbipd` and {command}`usbip`, for
          {option}`services.gradient.worker.exec.devices` entries with `usb`.
        '';
      };
    };

    exec = {
      image = lib.mkOption {
        type = lib.types.package;
        default = (pkgs.nixos {
          imports = [ ./guest.nix ];
          gradient.guest.enable = true;
        }).config.system.build.gradientGuest;
        defaultText = lib.literalExpression "(pkgs.nixos { imports = [ ./guest.nix ]; gradient.guest.enable = true; }).config.system.build.gradientGuest";
        example = lib.literalExpression "gradient.lib.mkWorkerImage { modules = [ ./worker-guest.nix ]; }";
        description = ''
          Guest image for exec jobs, built with `gradient.lib.mkWorkerImage`.
        '';
      };

      devices = lib.mkOption {
        type = lib.types.listOf (lib.types.submodule {
          options = {
            name = lib.mkOption {
              type = lib.types.str;
              example = "gpu:h100";
              description = "Device class name that exec jobs ask for.";
            };

            vfio = lib.mkOption {
              type = lib.types.listOf lib.types.str;
              default = [ ];
              example = [ "0000:17:00.0" "0000:18:00.0" ];
              description = ''
                PCI addresses of the devices in this class, a device per address. The worker
                hot-plugs them into the guest over VFIO. Bind each device to {file}`vfio-pci` first.
              '';
            };

            dev = lib.mkOption {
              type = lib.types.nullOr lib.types.path;
              default = null;
              apply = lib.mapNullable toString;
              example = "/dev/ttyUSB0";
              description = "Host device file for the guest, relayed over vsock.";
            };

            usb = lib.mkOption {
              type = lib.types.nullOr (lib.types.strMatching "[0-9a-f]{4}:[0-9a-f]{4}");
              default = null;
              example = "1d6b:0002";
              description = "USB device as `vendor:product`, attached to the guest over USB/IP.";
            };
          };
        });
        default = [ ];
        example = lib.literalExpression ''
          [
            { name = "gpu:h100"; vfio = [ "0000:17:00.0" "0000:18:00.0" ]; }
            { name = "kvm"; dev = "/dev/kvm"; }
            { name = "camera"; usb = "1d6b:0002"; }
          ]
        '';
        description = ''
          Devices that exec jobs can ask for. Set exactly `vfio`, `dev` or `usb` per entry.
          Devices held by a job stay out of reach for other jobs.
        '';
      };

      mounts = lib.mkOption {
        type = lib.types.listOf (lib.types.submodule {
          options = {
            name = lib.mkOption {
              type = lib.types.str;
              example = "datasets";
              description = "Mount name that exec jobs ask for. Guests find the mount at {file}`/mnt/<name>`.";
            };

            path = lib.mkOption {
              type = lib.types.path;
              apply = toString;
              example = "/srv/datasets";
              description = "Host directory for the guest, shared over virtiofs.";
            };

            readOnly = lib.mkOption {
              type = lib.types.bool;
              default = false;
              description = "Whether to share the directory read-only.";
            };

            perJob = lib.mkOption {
              type = lib.types.bool;
              default = false;
              description = ''
                Whether to give each job a fresh directory below `path`, deleted after the job.
              '';
            };
          };
        });
        default = [ ];
        example = lib.literalExpression ''
          [
            { name = "datasets"; path = "/srv/datasets"; readOnly = true; }
            { name = "scratch"; path = "/srv/scratch"; perJob = true; }
          ]
        '';
        description = "Host directories that exec jobs can ask for.";
      };

      allow.ports = lib.mkOption {
        type = lib.types.nullOr (lib.types.listOf lib.types.port);
        default = null;
        example = [ 1024 65535 ];
        description = ''
          First and last port of the range that exec jobs can ask for. `null` allows ports
          1024 to 65535.
        '';
      };
    };

    reverseProxy = {
      nginx.enable = lib.mkEnableOption "an nginx virtual host for the worker listener";
      caddy = {
        enable = lib.mkEnableOption "a Caddy virtual host for the worker listener";
        useACMEHost = lib.mkOption {
          type = lib.types.nullOr lib.types.str;
          default = null;
          description = ''
            Host of an existing ACME certificate, passed to
            {option}`services.caddy.virtualHosts.<name>.useACMEHost`. Caddy reuses it instead of
            requesting a new certificate. `null` keeps certificate management with Caddy.
          '';
        };

        extraConfig = lib.mkOption {
          type = lib.types.lines;
          default = "";
          description = ''
            Additional lines appended to {option}`services.caddy.virtualHosts.<name>.extraConfig`
            after the reverse proxy setup.
          '';
        };
      };
    };

    useTls = lib.mkEnableOption "TLS" // { default = true; };

    domain = lib.mkOption {
      type = lib.types.str;
      default = "";
      example = "worker.example.com";
      description = ''
        Domain of the worker's reverse proxy virtual host. Only applies with a reverse proxy
        enabled.
      '';
    };

    peersFile = lib.mkOption {
      type = lib.types.nullOr lib.types.path;
      default = null;
      description = ''
        File of peer tokens for challenge-response authentication with the server,
        with `peer_id:token` per line and `#` starting a comment.

        ```
        <uuid>:<token>
        *:<token>
        ```

        Use peer ID `*` to match any UUID in the server's challenge.
        Tokens are 48 byte random secrets, for example from {command}`openssl rand -base64 48`.
        Register them via the API through `POST /api/v1/projects/{project}/workers`.
        Set it to `null` for open mode without authentication; avoid that in a production setup.
      '';
    };

    acceptedServerTokensFile = lib.mkOption {
      type = lib.types.nullOr lib.types.path;
      default = null;
      description = ''
        File of token hashes required from connecting servers,
        with `peer_id:hash` per line and `#` starting a comment.
        A hash is an argon2 PHC string or the lowercase SHA-256 hex digest of the token itself,
        for example from {command}`printf %s "$TOKEN" | sha256sum`.
        Peer ID `*` matches any peer.
        The worker reads this file only with {env}`GRADIENT_WORKER_DISCOVERABLE`.
        Set it to `null` to accept every server; avoid that in a production setup.
      '';
    };

    system = {
      architectures = lib.mkOption {
        type = lib.types.listOf lib.types.str;
        default = [ pkgs.stdenv.hostPlatform.system ] ++ lib.optional (pkgs.stdenv.hostPlatform.system == "x86_64-linux") "i686-linux";
        defaultText = lib.literalExpression ''[ pkgs.stdenv.hostPlatform.system ] ++ lib.optional (pkgs.stdenv.hostPlatform.system == "x86_64-linux") "i686-linux"'';
        example = [ "x86_64-linux" "aarch64-linux" ];
        description = "Nix system types this worker builds for.";
      };

      features = lib.mkOption {
        type = lib.types.listOf lib.types.str;
        default = [ ];
        example = [ "nixos-test" "benchmark" "big-parallel" ];
        description = ''
          Nix system features this worker advertises. An empty list uses the system features of
          the Nix store, including those that enabled experimental features imply, such as
          `recursive-nix`.
        '';
      };

    };

    nixDaemon = {
      maxConnections = lib.mkOption {
        type = lib.types.ints.positive;
        default = cfg.build.maxConcurrent * 9 + 16;
        defaultText = lib.literalExpression "config.services.gradient.worker.build.maxConcurrent * 9 + 16";
        description = ''
          Maximum connections to the local Nix daemon. Each build holds one for its whole run
          plus up to 8 for parallel NAR imports. The rest is headroom for path checks.
        '';
      };
    };

    build = {
      metrics = lib.mkOption {
        type = lib.types.bool;
        default = false;
        description = ''
          Whether to record per-build peak memory, CPU time, disk I/O and out-of-memory kills.
          This turns on Nix's experimental `cgroups` feature and `use-cgroups` setting and
          delegates cgroup controllers to {file}`nix-daemon.service`. The daemon reports the
          values in each build result. Peak memory, disk I/O and out-of-memory kills need
          Gradient's Nix fork on the daemon, and {option}`nix.package` then defaults to its
          package. Wall-clock time is always recorded.
        '';
      };

      cgroup = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = if cfg.build.metrics then "/sys/fs/cgroup/system.slice/nix-daemon.service" else null;
        defaultText = lib.literalExpression ''
          if config.services.gradient.worker.build.metrics
          then "/sys/fs/cgroup/system.slice/nix-daemon.service"
          else null
        '';
        description = ''
          Cgroup directory of {file}`nix-daemon.service`, the parent of the build cgroups. The
          worker reads each running build's peak memory from it and sends the values with its
          heartbeat. The scheduler reserves the larger of the predicted and the reported peak.
          The build cgroups need {option}`services.gradient.worker.build.metrics` and Gradient's
          Nix fork on the daemon. `null` reports nothing.
        '';
      };

      oomScoreAdjust = lib.mkOption {
        type = lib.types.nullOr (lib.types.ints.between (-1000) 1000);
        default = 500;
        description = ''
          Out-of-memory score adjustment for {file}`nix-daemon.service`, inherited by every build
          process. A positive value makes the kernel kill a build process before the worker when
          the host runs out of memory, failing only that build. Evaluation subprocesses use 600
          and die first. `null` leaves the daemon unchanged.
        '';
      };
    };

    nar = {
      chunkBytes = lib.mkOption {
        type = lib.types.ints.between 4096 4194304;
        default = 524288;
        description = ''
          Size in bytes of each outbound NAR upload chunk, from 4 KiB to 4 MiB.
          Smaller chunks can help slow or high-latency links like Tailscale over DERP.
        '';
      };
    };

    log = {
      level = lib.mkOption {
        type = lib.types.submodule {
          options = {
            default = lib.mkOption {
              type = logLevelType;
              default = "info";
              description = "Default log level.";
            };

            eval = lib.mkOption {
              type = lib.types.nullOr logLevelType;
              default = null;
              description = ''
                Log level of the evaluator. `null` uses
                {option}`services.gradient.worker.log.level.default`.
              '';
            };

            build = lib.mkOption {
              type = lib.types.nullOr logLevelType;
              default = null;
              description = ''
                Log level of the builder. `null` uses
                {option}`services.gradient.worker.log.level.default`.
              '';
            };

            proto = lib.mkOption {
              type = lib.types.nullOr logLevelType;
              default = null;
              description = ''
                Log level of the protocol layer. `null` uses
                {option}`services.gradient.worker.log.level.default`.
              '';
            };
          };
        };
        default = { };
        description = "Log levels per component.";
      };

    };
  };

  config = lib.mkIf cfg.enable (lib.mkMerge [ {
    assertions = [
      {
        assertion = cfg.serverUrl != null || cfg.discoverable;
        message = "services.gradient.worker: serverUrl unset without discoverable, set either to connect to a server";
      }
      {
        assertion = cfg.capabilities.federate -> cfg.discoverable;
        message = "services.gradient.worker: capabilities.federate and discoverable together, enable both";
      }
      {
        assertion = !(cfg.reverseProxy.nginx.enable && cfg.reverseProxy.caddy.enable);
        message = "services.gradient.worker: a reverse proxy at a time, nginx or caddy, not both";
      }
      {
        assertion = !(cfg.system.minFreeRamMb != 0 && cfg.eval.maxMemory != null);
        message = "services.gradient.worker.system.minFreeRamMb and services.gradient.worker.eval.maxMemory are alternatives, unset either";
      }
      {
        assertion = execOptionsSet -> cfg.capabilities.exec;
        message = "The services.gradient.worker.exec options depend on capabilities.exec. Enable capabilities.exec or unset services.gradient.worker.exec.";
      }
      {
        assertion = exec.allow.ports == null || lib.length exec.allow.ports == 2;
        message = "services.gradient.worker.exec.allow.ports: a list of the first and last port, not ${toString (lib.length exec.allow.ports)} ports";
      }
    ];

    systemd = {
      tmpfiles.settings."10-gradient" = lib.optionalAttrs (cfg.gcrootsDir != "") {
        ${cfg.gcrootsDir}.d = {
          user = "gradient-worker";
          group = "gradient-worker";
          mode = "0755";
        };
      } // lib.optionalAttrs (cfg.log.traceDir != null) {
        ${cfg.log.traceDir}.d = {
          user = "gradient-worker";
          group = "gradient-worker";
          mode = "0750";
        };
      };

      services = {
        nix-daemon.serviceConfig = {
          Delegate = lib.mkIf cfg.build.metrics true;
          OOMScoreAdjust = lib.mkIf (cfg.build.oomScoreAdjust != null) (
            lib.mkDefault cfg.build.oomScoreAdjust
          );
        };

        gradient-worker = {
          wantedBy = [ "multi-user.target" ];
          after = [ "network.target" ];
          path = [
            cfg.packages.git
            cfg.packages.ssh
          ] ++ lib.optional cfg.capabilities.exec pkgs.iproute2
            ++ lib.optional (cfg.capabilities.exec && hasDevice "usb") cfg.packages.usbip;

          serviceConfig = {
            Type = "notify";
            ExecStart = lib.getExe' cfg.packages.gradient "gradient-worker";
            StateDirectory = "gradient-worker";
            User = "gradient-worker";
            Group = "gradient-worker";
            PrivateTmp = true;
            ProtectHome = true;
            ProtectHostname = true;
            ProtectKernelLogs = true;
            ProtectKernelModules = true;
            ProtectKernelTunables = true;
            ProtectProc = "invisible";
            ProtectSystem = "strict";
            ReadWritePaths = lib.optionals (cfg.gcrootsDir != "") [ cfg.gcrootsDir ]
              ++ lib.optional (cfg.log.traceDir != null) cfg.log.traceDir
              ++ map (mount: mount.path) (lib.filter (mount: mount.perJob) exec.mounts);
            Restart = "on-failure";
            RestartSec = 10;
            KillMode = "mixed";
            LimitNOFILE = 65535;
            RestrictAddressFamilies = [ "AF_INET" "AF_INET6" "AF_UNIX" ];
            RestrictNamespaces = true;
            RestrictRealtime = true;
            RestrictSUIDSGID = true;
            WorkingDirectory = cfg.baseDir;
            LoadCredential = lib.optionals (cfg.peersFile != null) [
              "gradient_worker_peers:${cfg.peersFile}"
            ] ++ lib.optionals (cfg.acceptedServerTokensFile != null) [
              "gradient_worker_accepted_server_tokens:${cfg.acceptedServerTokensFile}"
            ];
          };

          environment = {
            NIX_REMOTE = "daemon";
            XDG_CACHE_HOME = "${cfg.baseDir}/www/.cache";
            RUST_LOG = cfg.log.level.default;
            GRADIENT_WORKER_SSH_BIN = lib.getExe' cfg.packages.ssh "ssh";
          } // lib.optionalAttrs (cfg.peersFile != null) {
            GRADIENT_WORKER_PEERS_FILE = "%d/gradient_worker_peers";
          } // lib.optionalAttrs (cfg.acceptedServerTokensFile != null) {
            GRADIENT_WORKER_ACCEPTED_SERVER_TOKENS_FILE = "%d/gradient_worker_accepted_server_tokens";
          } // lib.optionalAttrs cfg.capabilities.exec {
            GRADIENT_WORKER_EXEC_IMAGE = "${exec.image}";
            GRADIENT_WORKER_EXEC_DEVICES = builtins.toJSON exec.devices;
            GRADIENT_WORKER_EXEC_MOUNTS = builtins.toJSON exec.mounts;
            GRADIENT_WORKER_EXEC_CLOUD_HYPERVISOR_BIN = lib.getExe' cfg.packages.cloudHypervisor "cloud-hypervisor";
            GRADIENT_WORKER_EXEC_VIRTIOFSD_BIN = lib.getExe cfg.packages.virtiofsd;
          } // lib.optionalAttrs (cfg.capabilities.exec && exec.allow.ports != null) {
            GRADIENT_WORKER_EXEC_ALLOW_PORTS = lib.concatMapStringsSep "," toString exec.allow.ports;
          } // workerOptions.environment cfg // lib.mapAttrs (_: environmentValue) cfg.environmentVariables;
        };
      };
    };

    services = {
      nginx = lib.mkIf cfg.reverseProxy.nginx.enable {
        enable = true;
        virtualHosts."${cfg.domain}" = {
          enableACME = lib.mkDefault cfg.useTls;
          forceSSL = lib.mkDefault cfg.useTls;
          locations."/proto" = {
            proxyPass = "http://${cfg.listenAddr}:${toString cfg.port}";
            proxyWebsockets = true;
            extraConfig = ''
              proxy_buffer_size 256k;
              proxy_buffers 4 256k;
              proxy_connect_timeout 90d;
              proxy_send_timeout 90d;
              proxy_read_timeout 90d;
            '';
          };
        };
      };

      caddy = lib.mkIf cfg.reverseProxy.caddy.enable {
        enable = true;
        virtualHosts."${if cfg.useTls then "" else "http://"}${cfg.domain}" = {
          inherit (cfg.reverseProxy.caddy) useACMEHost;
          extraConfig = ''
            reverse_proxy http://${cfg.listenAddr}:${toString cfg.port}
          '';
        };
      };
    };

    nix = {
      package = lib.mkIf cfg.build.metrics (lib.mkDefault pkgs.gradient-nix);
      settings = {
        trusted-users = [ "gradient-worker" ];
        use-cgroups = lib.mkIf cfg.build.metrics true;
        experimental-features = [
          "nix-command"
          "flakes"
          "ca-derivations"
        ] ++ lib.optional cfg.build.metrics "cgroups";
      };
    };


    users = {
      groups.gradient-worker = { };
      users.gradient-worker = {
        description = "Gradient Worker user";
        isSystemUser = true;
        group = "gradient-worker";
      };
    };
  } execConfig ]);
}
