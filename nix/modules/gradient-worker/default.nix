/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ lib, pkgs, config, options, ... }: let
  cfg = config.services.gradient.worker;
  exec = cfg.exec;

  subnetParts = lib.splitString "/" exec.subnet.v4;
  prefixLength = lib.toInt (lib.last subnetParts);
  addressToInt = address: lib.foldl (sum: octet: sum * 256 + lib.toInt octet) 0 (lib.splitString "." address);
  intToAddress = value: lib.concatMapStringsSep "." (place: toString (lib.mod (value / place) 256)) [ 16777216 65536 256 1 ];
  twoToThePowerOf = exponent: lib.foldl (power: _: power * 2) 1 (lib.range 1 exponent);
  subnetSize = twoToThePowerOf (32 - prefixLength);
  subnetStart = addressToInt (lib.head subnetParts);
  bridgeAddress = intToAddress (subnetStart - lib.mod subnetStart subnetSize + 1);
  guestIpv6SubnetParts = lib.network.ipv6.fromString exec.subnet.v6;
  guestSubnetIpv6AddressGroupsAsIntegers = map lib.fromHexString (lib.splitString ":" guestIpv6SubnetParts.address);
  ipv6GuestSubnetHostBitCountForGroup = groupIndex: lib.max 0 (lib.min 16 (16 * (groupIndex + 1) - guestIpv6SubnetParts.prefixLength));
  ipv6GuestSubnetHasZeroHostBits = lib.all lib.id (lib.imap0
    (groupIndex: group: lib.mod group (twoToThePowerOf (ipv6GuestSubnetHostBitCountForGroup groupIndex)) == 0)
    guestSubnetIpv6AddressGroupsAsIntegers);
  guestIPv6NetworkAddressText = lib.concatStringsSep ":" (lib.imap0
    (groupIndex: group: lib.toLower (lib.toHexString (group - lib.mod group (twoToThePowerOf (ipv6GuestSubnetHostBitCountForGroup groupIndex)))))
    guestSubnetIpv6AddressGroupsAsIntegers);
  guestIPv6SubnetLongestPrefixLength = 96 + prefixLength;
  guestBridgeIPv6Address = lib.concatMapStringsSep ":" (group: lib.toLower (lib.toHexString group))
    (lib.init guestSubnetIpv6AddressGroupsAsIntegers ++ [ (lib.last guestSubnetIpv6AddressGroupsAsIntegers + 1) ]);
  ipv6NetworkCidrRegularExpressionText = "[0-9a-fA-F:]*:[0-9a-fA-F:]*/[0-9]{1,3}";
  execOptionsSet = lib.any (option: option.highestPrio < (lib.mkOptionDefault null).priority)
    (lib.collect lib.isOption options.services.gradient.worker.exec);
  guestDnsForwarder = exec.allow.network && exec.nameservers == [ ];
  hasDevice = source: lib.any (device: device.${source} != null && device.${source} != [ ]) exec.devices;
  renderedRulesetTexts = import ./exec-guest-network-filter-nftables-ruleset-texts.nix {
    inherit lib bridgeAddress;
    inherit (exec) bridge;
    inherit (exec.allow) allowedPrivateNetworks blockedGlobalIPv6Networks guestConnectableHostTcpPorts;
    guestDnsForwarderEnabled = guestDnsForwarder;
    execGuestHasIpv6Address = exec.subnet.v6 != null;
  };
  nftablesServiceLoadsHostFirewall = config.networking.nftables.enable;
  guestNftablesLoadCommand = pkgs.writeShellScript "gradient-guest-nftables-load" ''
    export PATH=${lib.makeBinPath [ pkgs.coreutils pkgs.nftables pkgs.iproute2 pkgs.util-linux ]}
    ${renderedRulesetTexts.reloadGuestNftablesScriptTextKeepingAddressesOfRunningGuests {
      guestTapAddressRecordDirectory = "/run/gradient-guest-tap-addresses";
      guestNetworkFilterNftablesScriptFilePath = pkgs.writeText "gradient-guest-nftables-load.nft" renderedRulesetTexts.atomicNftablesTableReloadScriptKeepingGuestAddressSet;
    }}
  '';
  execMountEntriesWithInvalidOrReservedName = lib.filter
    (mount: builtins.match "[A-Za-z0-9_-]{1,64}" mount.name == null || lib.elem mount.name [ "work" "control" "console" ])
    exec.mounts;
  execMountNamesUsedMoreThanOnce = lib.attrNames
    (lib.filterAttrs (_: count: count > 1)
      (lib.foldl' (counts: mount: counts // { ${mount.name} = (counts.${mount.name} or 0) + 1; }) { } exec.mounts));

  execConfig = lib.mkIf cfg.capabilities.exec {
    security.polkit = {
      enable = true;
      extraConfig = ''
        polkit.addRule(function(action, subject) {
          if (action.id == "org.freedesktop.systemd1.manage-units" &&
              subject.user == "gradient-worker" &&
              ["start", "stop"].indexOf(action.lookup("verb")) >= 0 &&
              /^gradient-guest@[0-9a-f]{32}\.service$/.test(action.lookup("unit") || "")) {
            return polkit.Result.YES;
          }
        });
      '';
    };

    users.users.gradient-worker.extraGroups = [ "kvm" ];

    systemd = {
      tmpfiles.rules = map (mount: "d ${mount.path} 0770 gradient-worker gradient-worker -")
        (lib.filter (mount: mount.perJob) exec.mounts);

      services = {
        usbipd = lib.mkIf (hasDevice "usb") {
          description = "USB/IP server for Gradient exec job devices";
          wantedBy = [ "multi-user.target" ];
          after = [ "network.target" ];
          serviceConfig = {
            ExecStart = "${lib.getExe' cfg.packages.usbip "usbipd"} -4";
            IPAddressAllow = "localhost";
            IPAddressDeny = "any";
            LockPersonality = true;
            MemoryDenyWriteExecute = true;
            NoNewPrivileges = true;
            PrivateTmp = true;
            ProtectHome = true;
            ProtectSystem = "full";
            RestrictAddressFamilies = [ "AF_INET" "AF_UNIX" "AF_NETLINK" ];
            RestrictNamespaces = true;
            RestrictRealtime = true;
            SystemCallArchitectures = "native";
          };
        };

        "gradient-guest@" = {
          description = "Root supervisor for Gradient guest %i";
          path = [ pkgs.iproute2 pkgs.nftables ] ++ lib.optional (hasDevice "usb") cfg.packages.usbip;
          environment = lib.filterAttrs (name: _: lib.hasPrefix "GRADIENT_WORKER_EXEC_" name)
            config.systemd.services.gradient-worker.environment;
          bindsTo = [ "gradient-guest-vm@%i.service" ];
          wants = [ "gradient-guest-nftables.service" ];
          after = [ "gradient-guest-nftables.service" ];
          unitConfig.CollectMode = "inactive-or-failed";
          serviceConfig = {
            ExecStart = "${lib.getExe' cfg.packages.gradient "gradient-worker"} --guest-supervisor ${cfg.baseDir}/guests/%i";
            ExecStopPost = "${lib.getExe' cfg.packages.gradient "gradient-worker"} --release-guest-taps %i";
            # systemd services create `/run/gradient-guests/<guest id>` with owner `root:gradient-worker`
            # and mode `0750`. Supervisor processes use `UMask 0007` for the sockets and `console.log`
            # in that directory. The worker and cloud-hypervisor can connect to the sockets but
            # cannot add or replace files.
            RuntimeDirectory = "gradient-guests/%i";
            RuntimeDirectoryMode = "0750";
            Group = "gradient-worker";
            UMask = "0007";
            KillMode = "control-group";
            Slice = "gradient-guests.slice";
            NoNewPrivileges = true;
            LockPersonality = true;
            ProtectClock = true;
            ProtectHostname = true;
            ProtectKernelLogs = true;
            RestrictRealtime = true;
            RestrictSUIDSGID = true;
          };
        };

        gradient-guest-nftables = {
          description = "nftables tables that isolate Gradient guest networks";
          wantedBy = [ "multi-user.target" ] ++ lib.optional nftablesServiceLoadsHostFirewall "nftables.service";
          after = lib.optional nftablesServiceLoadsHostFirewall "nftables.service";
          partOf = lib.optional nftablesServiceLoadsHostFirewall "nftables.service";
          reloadIfChanged = true;
          unitConfig = lib.mkIf nftablesServiceLoadsHostFirewall { ReloadPropagatedFrom = "nftables.service"; };
          serviceConfig = {
            Type = "oneshot";
            RemainAfterExit = true;
            RuntimeDirectory = "gradient-guest-tap-addresses";
            RuntimeDirectoryMode = "0700";
            RuntimeDirectoryPreserve = true;
            ExecStart = guestNftablesLoadCommand;
            ExecReload = guestNftablesLoadCommand;
          };
        };

        nftables = lib.mkIf (nftablesServiceLoadsHostFirewall && exec.allow.network) {
          serviceConfig = let
            guestNftablesReloadCommandsOnHostFirewallStartStop = [ "-${lib.getExe' pkgs.coreutils "timeout"} 10 ${guestNftablesLoadCommand}" ];
          in {
            ExecStartPost = lib.mkAfter guestNftablesReloadCommandsOnHostFirewallStartStop;
            ExecStopPost = guestNftablesReloadCommandsOnHostFirewallStartStop;
          };
        };

        "gradient-guest-vm@" = {
          description = "Unprivileged cloud-hypervisor for Gradient guest %i";
          bindsTo = [ "gradient-guest@%i.service" ];
          after = [ "gradient-guest@%i.service" ];
          unitConfig.CollectMode = "inactive-or-failed";
          serviceConfig = {
            ExecStart = "${lib.getExe' cfg.packages.cloudHypervisor "cloud-hypervisor"} --api-socket path=${cfg.baseDir}/guests/%i/api.sock";
            User = "gradient-worker";
            Group = "gradient-worker";
            SupplementaryGroups = [ "kvm" ];
            UMask = "0007";
            LimitMEMLOCK = "infinity";
            KillMode = "control-group";
            Slice = "gradient-guests.slice";
            CapabilityBoundingSet = "";
            NoNewPrivileges = true;
            DevicePolicy = "closed";
            DeviceAllow = [ "/dev/kvm rw" "/dev/net/tun rw" "/dev/vfio/vfio rw" "char-vfio rw" ];
            ProtectSystem = "strict";
            ReadWritePaths = [ "${cfg.baseDir}/guests/%i" "/run/gradient-guests/%i" ];
            ProtectHome = true;
            PrivateTmp = true;
            ProtectClock = true;
            ProtectControlGroups = true;
            ProtectHostname = true;
            ProtectKernelLogs = true;
            ProtectKernelModules = true;
            LockPersonality = true;
            RestrictRealtime = true;
            RestrictSUIDSGID = true;
          };
        };

        gradient-guest-dns = lib.mkIf guestDnsForwarder {
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
            DynamicUser = true;
            AmbientCapabilities = [ "CAP_NET_BIND_SERVICE" ];
            CapabilityBoundingSet = [ "CAP_NET_BIND_SERVICE" ];
            LockPersonality = true;
            MemoryDenyWriteExecute = true;
            NoNewPrivileges = true;
            PrivateDevices = true;
            ProtectClock = true;
            ProtectControlGroups = true;
            ProtectHome = true;
            ProtectHostname = true;
            ProtectKernelLogs = true;
            ProtectKernelModules = true;
            ProtectKernelTunables = true;
            ProtectProc = "invisible";
            ProtectSystem = "strict";
            RestrictAddressFamilies = [ "AF_INET" "AF_INET6" "AF_UNIX" "AF_NETLINK" ];
            RestrictNamespaces = true;
            RestrictRealtime = true;
            SystemCallArchitectures = "native";
            SystemCallFilter = [ "@system-service" ];
          };
        };
      };
    };

    services.udev.extraRules = lib.mkIf (hasDevice "vfio") ''
      SUBSYSTEM=="vfio", GROUP="gradient-worker", MODE="0660"
    '';

    boot = {
      kernelModules = lib.mkIf (hasDevice "usb") [ "usbip-host" ];

      kernel.sysctl = lib.mkIf (exec.subnet.v6 != null) {
        "net.ipv6.conf.${exec.bridge}.accept_ra" = 0;
        "net.ipv6.conf.${exec.bridge}.accept_dad" = 0;
      };
    };

    networking = {
      bridges.${exec.bridge}.interfaces = [ ];
      interfaces.${exec.bridge} = {
        macAddress = lib.mkDefault "06:47:52:00:00:01";
        ipv4.addresses = [{
          address = bridgeAddress;
          inherit prefixLength;
        }];
        ipv6.addresses = lib.optional (exec.subnet.v6 != null) {
          address = guestBridgeIPv6Address;
          inherit (guestIpv6SubnetParts) prefixLength;
        };
      };
      nat = lib.mkIf exec.allow.network {
        enable = true;
        enableIPv6 = exec.subnet.v6 != null;
        internalInterfaces = [ exec.bridge ];
      };
      firewall.interfaces.${exec.bridge} = {
        allowedUDPPorts = lib.optional guestDnsForwarder 53;
        allowedTCPPorts = lib.optional guestDnsForwarder 53 ++ exec.allow.guestConnectableHostTcpPorts;
      };
    };
  };

  sharedWorkerOptionsModule = import ./options-assertions-environment.nix { inherit lib pkgs cfg; };
in {
  disabledModules = [ "services/continuous-integration/gradient/worker.nix" ];

  imports = [
    { options.services.gradient.worker = sharedWorkerOptionsModule.options; }
  ];

  options.services.gradient.worker = {
    packages = {
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
          imports = [ ../guest.nix ];
          gradient.guest.enable = true;
        }).config.system.build.gradientGuest;
        defaultText = lib.literalExpression "(pkgs.nixos { imports = [ ./guest.nix ]; gradient.guest.enable = true; }).config.system.build.gradientGuest";
        example = lib.literalExpression "gradient.lib.mkWorkerImage { modules = [ ./worker-guest.nix ]; }";
        description = ''
          Guest image for exec jobs, built with `gradient.lib.mkWorkerImage`.
        '';
      };

      noSwap = lib.mkOption {
        type = lib.types.bool;
        default = false;
        description = ''
          Whether to keep the worker service and `gradient-guests.slice` out of swap. Guest
          memory and the secrets in it then stay in RAM. Under memory pressure the kernel kills
          guests and worker processes instead of swapping them out.
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

            projects = sharedWorkerOptionsModule.workerResourceProjectsOption;
          };
        });
        default = [ ];
        example = lib.literalExpression ''
          [
            { name = "gpu:h100"; vfio = [ "0000:17:00.0" "0000:18:00.0" ]; projects = "all"; }
            { name = "kvm"; dev = "/dev/kvm"; projects = "all"; }
            { name = "camera"; usb = "1d6b:0002"; projects = "all"; }
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

            projects = sharedWorkerOptionsModule.workerResourceProjectsOption;

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
            { name = "datasets"; path = "/srv/datasets"; projects = "all"; readOnly = true; }
            { name = "scratch"; path = "/srv/scratch"; projects = "all"; perJob = true; }
          ]
        '';
        description = "Host directories that exec jobs can ask for.";
      };

      allow = {
        ports = lib.mkOption {
          type = lib.types.nullOr (lib.types.listOf (lib.types.either lib.types.port (lib.types.listOf lib.types.port)));
          default = null;
          example = lib.literalExpression "[ 22 [ 8000 8100 ] 6006 ]";
          description = ''
            Guest ports that exec jobs can request. Each entry can be a single port or `[ first last ]` range. No ports pass with an empty list. `null` stands for ports 1024 to 65535.
          '';
        };

        allowedPrivateNetworks = lib.mkOption {
          type = lib.types.listOf (lib.types.strMatching "[0-9]{1,3}(\\.[0-9]{1,3}){3}/[0-9]{1,2}|${ipv6NetworkCidrRegularExpressionText}");
          default = [ ];
          example = [ "10.234.0.0/16" "192.168.10.5/32" "fd00:ec2:7::/64" ];
          description = ''
            Use this option to allow specific private IPv4 and IPv6 networks.
            Guest network filters drop per default private, shared and link-local destinations (10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16, 100.64.0.0/10, 169.254.0.0/16, etc.) and all IPv6 destinations outside 2000::/3.
            Guests with network access can still reach networks explicitly allowed in this option.
            IPv6 entries only have an effect when exec.subnet.v6 is set.
            Add the guest subnets of other workers for multi-node jobs.
            Add private hosts that the job needs.
            Guests on this worker cannot reach each other, even if those subnets appear in the list.
          '';
        };

        blockedGlobalIPv6Networks = lib.mkOption {
          type = lib.types.listOf (lib.types.strMatching ipv6NetworkCidrRegularExpressionText);
          default = [ ];
          example = [ "2001:db8:1234::/48" ];
          description = ''
            IPv6 networks that a guest is forbidden to connect to.
            Set this option together with `exec.subnet.v6`.
            The global IPv6 prefixes of the host's own network (eg: the hosts LAN prefix) should be set here, too.
            The default of `[ ]` blocks nothing.
            This option is ineffective without `exec.subnet.v6`.
          '';
        };

        guestConnectableHostTcpPorts = lib.mkOption {
          type = lib.types.listOf lib.types.port;
          default = [ ];
          example = [ 3000 ];
          description = ''
            TCP ports on the worker host that a guest with network access can reach.
            Guest network filters drop all other connections from a guest to the host.
            A DNS forwarder for guests on the host makes DNS to the bridge address the exception.
            These ports are also open on the host firewall for the guest bridge.
            Set this option when the Gradient server and the jobs share a host and the jobs need to reach its API.
          '';
        };
      };

      sops.keyFile = lib.mkOption {
        type = lib.types.nullOr lib.types.path;
        default = null;
        example = "/etc/ssh/ssh_host_ed25519_key";
        description = ''
          SSH private key decrypting {option}`services.gradient.worker.exec.sops.files`, an
          Ed25519 or RSA key. The age recipient derives from it.
          The worker reads it through a systemd credential, and
          {command}`systemctl reload gradient-worker` decrypts the files again. `null` decrypts
          no file.
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
  };

  config = lib.mkIf cfg.enable (lib.mkMerge [ {
    assertions = sharedWorkerOptionsModule.assertions ++ [
      {
        assertion = config.systemd.services.gradient-worker.enable -> cfg.serverUrl != null || cfg.discoverable;
        message = "services.gradient.worker: serverUrl unset without discoverable, set either to connect to a server";
      }
      {
        assertion = !(cfg.reverseProxy.nginx.enable && cfg.reverseProxy.caddy.enable);
        message = "services.gradient.worker: a reverse proxy at a time, nginx or caddy, not both";
      }
      {
        assertion = cfg.reverseProxy.nginx.enable || cfg.reverseProxy.caddy.enable -> cfg.domain != "";
        message = "Set `services.gradient.worker.domain` if the reverse proxy is active.";
      }
      {
        assertion = execOptionsSet -> cfg.capabilities.exec;
        message = "The services.gradient.worker.exec options depend on capabilities.exec. Enable capabilities.exec or unset services.gradient.worker.exec.";
      }
      {
        assertion = exec.allow.ports == null || lib.all (entry: !lib.isList entry || (lib.length entry == 2 && lib.head entry <= lib.last entry)) exec.allow.ports;
        message = "services.gradient.worker.exec.allow.ports: Write each range in the form [ first last ]. Ensure first <= last.";
      }
      {
        assertion = cfg.capabilities.exec -> exec.subnet.v4 != null;
        message = "services.gradient.worker.exec.subnet.v4: capabilities.exec is on, but this option has no value. Set an IPv4 subnet that other networks on the host don't use.";
      }
      {
        assertion = exec.subnet.v6 != null -> exec.subnet.v4 != null;
        message = "A setting of `services.gradient.worker.exec.subnet.v6` without `services.gradient.worker.exec.subnet.v4` is invalid. Guest IPv6 addresses derive from IPv4 addresses. Set `services.gradient.worker.exec.subnet.v4` as well.";
      }
      {
        assertion = exec.subnet.v6 != null && exec.subnet.v4 != null
          -> guestIpv6SubnetParts.prefixLength >= 64 && guestIpv6SubnetParts.prefixLength <= guestIPv6SubnetLongestPrefixLength;
        message = "services.gradient.worker.exec.subnet.v6: Use a prefix length between 64 and ${toString guestIPv6SubnetLongestPrefixLength} for subnet ${exec.subnet.v6}.";
      }
      {
        assertion = exec.subnet.v6 != null -> ipv6GuestSubnetHasZeroHostBits;
        message = "services.gradient.worker.exec.subnet.v6: Host bits in subnet ${exec.subnet.v6} are non-zero. Write the network address ${guestIPv6NetworkAddressText}/${toString guestIpv6SubnetParts.prefixLength}.";
      }
      {
        assertion = exec.sops.files != [ ] -> exec.sops.keyFile != null;
        message = "services.gradient.worker.exec.sops.files: set without exec.sops.keyFile";
      }
      {
        assertion = execMountEntriesWithInvalidOrReservedName == [ ];
        message = "services.gradient.worker.exec.mounts: invalid mount name for ${toString (map (mount: mount.name) execMountEntriesWithInvalidOrReservedName)}. Mount names can contain 1 to 64 ASCII letters, digits, -, and _. The names work, control, and console belong to the worker.";
      }
      {
        assertion = execMountNamesUsedMoreThanOnce == [ ];
        message = "services.gradient.worker.exec.mounts: the mount names ${toString execMountNamesUsedMoreThanOnce} appear on more than one entry. Each mount needs its own name.";
      }
    ];

    systemd = {
      tmpfiles.settings."10-gradient" = {
        ${cfg.baseDir}.d = {
          user = "gradient-worker";
          group = "gradient-worker";
          mode = "0750";
        };
      } // lib.optionalAttrs (cfg.gcrootsDir != "") {
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

      slices = lib.mkIf cfg.exec.noSwap {
        gradient-guests.sliceConfig.MemorySwapMax = 0;
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
          after = [ "network.target" "nix-daemon.socket" "nix-daemon.service" ];
          path = [
            cfg.packages.git
            cfg.packages.ssh
          ] ++ lib.optionals cfg.capabilities.exec [ pkgs.iproute2 pkgs.e2fsprogs ]
            ++ lib.optional (cfg.capabilities.exec && hasDevice "usb") cfg.packages.usbip;

          serviceConfig = {
            Type = "notify";
            ExecStart = lib.getExe' cfg.packages.gradient "gradient-worker";
            ExecReload = lib.mkIf cfg.capabilities.exec "${pkgs.coreutils}/bin/kill -HUP $MAINPID";
            User = "gradient-worker";
            Group = "gradient-worker";
            UMask = "0027";
            CapabilityBoundingSet = "";
            LockPersonality = true;
            NoNewPrivileges = true;
            PrivateTmp = true;
            ProtectControlGroups = true;
            ProtectHome = true;
            ProtectHostname = true;
            ProtectKernelLogs = true;
            ProtectKernelModules = true;
            ProtectKernelTunables = true;
            ProtectProc = "invisible";
            ProtectSystem = "strict";
            ReadWritePaths = [ cfg.baseDir ]
              ++ lib.optional (cfg.gcrootsDir != "") cfg.gcrootsDir
              ++ lib.optional (cfg.log.traceDir != null) cfg.log.traceDir
              ++ map (mount: mount.path) (lib.filter (mount: mount.perJob) exec.mounts);
            Restart = "on-failure";
            RestartSec = 10;
            KillMode = "mixed";
            LimitNOFILE = 65535;
            MemorySwapMax = lib.mkIf cfg.exec.noSwap 0;
            RestrictAddressFamilies = [ "AF_INET" "AF_INET6" "AF_NETLINK" "AF_UNIX" ];
            RestrictNamespaces = true;
            RestrictRealtime = true;
            RestrictSUIDSGID = true;
            SystemCallArchitectures = "native";
            SystemCallFilter = [ "@system-service" ];
            WorkingDirectory = cfg.baseDir;
            LoadCredential = lib.optionals (cfg.peersFile != null) [
              "gradient_worker_peers:${cfg.peersFile}"
            ] ++ lib.optionals (cfg.acceptedServerTokensFile != null) [
              "gradient_worker_accepted_server_tokens:${cfg.acceptedServerTokensFile}"
            ] ++ lib.optionals (cfg.capabilities.exec && exec.sops.keyFile != null) [
              "gradient_worker_exec_sops_key:${exec.sops.keyFile}"
            ];
          };

          environment = lib.optionalAttrs (cfg.peersFile != null) {
            GRADIENT_WORKER_PEERS_FILE = "%d/gradient_worker_peers";
          } // lib.optionalAttrs (cfg.acceptedServerTokensFile != null) {
            GRADIENT_WORKER_ACCEPTED_SERVER_TOKENS_FILE = "%d/gradient_worker_accepted_server_tokens";
          } // lib.optionalAttrs cfg.capabilities.exec {
            GRADIENT_WORKER_EXEC_IMAGE = "${exec.image}";
            GRADIENT_WORKER_EXEC_DEVICES = builtins.toJSON exec.devices;
            GRADIENT_WORKER_EXEC_MOUNTS = builtins.toJSON exec.mounts;
            GRADIENT_WORKER_EXEC_VIRTIOFSD_BIN = lib.getExe cfg.packages.virtiofsd;
          } // lib.optionalAttrs (cfg.capabilities.exec && exec.sops.keyFile != null) {
            GRADIENT_WORKER_EXEC_SOPS_KEY_FILE = "%d/gradient_worker_exec_sops_key";
          } // lib.optionalAttrs (cfg.capabilities.exec && exec.allow.ports != null) {
            GRADIENT_WORKER_EXEC_ALLOW_PORTS = lib.concatMapStringsSep "," (entry: if lib.isList entry then "${toString (lib.head entry)}-${toString (lib.last entry)}" else toString entry) exec.allow.ports;
          } // lib.optionalAttrs (cfg.build.cgroup != null) {
            GRADIENT_WORKER_BUILD_CGROUP = cfg.build.cgroup;
          } // sharedWorkerOptionsModule.environment;
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
            recommendedProxySettings = false;
            extraConfig = ''
              proxy_set_header X-Forwarded-For $remote_addr;
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
            handle /proto {
              reverse_proxy http://${cfg.listenAddr}:${toString cfg.port}
            }
            handle {
              respond 404
            }

            ${cfg.reverseProxy.caddy.extraConfig}
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
