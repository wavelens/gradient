{ config, pkgs, ... }:
{
  
  systemd.services."serial-getty@".serviceConfig.ExecStartPre = "${pkgs.systemd}/bin/networkctl status br0";
  services.getty.greetingLine = "EXTREMELY UNSECURE SERVER, DO NOT STORE ANY IMPORTNANT DATA ON IT!!!";
  users.users.root.password = "root";
  system.stateVersion = config.system.nixos.version;
  microvm = {
    vcpu = 4;
    mem = 4096;
    writableStoreOverlay = "/nix/.rw-store";
    # hypervisor = "cloud-hypervisor";
    shares = [
      {
        tag = "store";
        source = "/nix/store";
        mountPoint = "/nix/.ro-store";
        # proto = "virtiofs";
      }
      # {
      #   tag = "root";
      #   source = "/tmp/gradient/${config.networking.hostName}";
      #   mountPoint = "/";
      #   # proto = "virtiofs";
      # }

    ];

    # # Persistent Storage
    # volumes = [{
    #     image = "gradient-persist.img";
    #     mountPoint = "/";
    #     size = 1024; # 1GB
    # }];

    interfaces = [{
      id = "eth0";
      type = "bridge";
      mac = "02:01:00:00:00:01";
      bridge = "virbr0";
    }];
  };

  networking = {
    hostName = "gradient-dev";
    useDHCP = false;
    nftables.enable = true;
    useNetworkd = true;
  };

  systemd.network = {
    netdevs = {
      "br0" = {
        netdevConfig = {
          Kind = "bridge";
          Name = "br0";
        };
      };
    };

    networks = {
      # uplink
      "00-eth" = {
        matchConfig.MACAddress = (builtins.head config.microvm.interfaces).mac;
        networkConfig.Bridge = "br0";
      };
      # bridge is a dumb switch without addresses on the host
      "01-br0" = {
        matchConfig.Name = "br0";
        networkConfig = {
          DHCP = "ipv4";
          IPv6AcceptRA = true;
        };

        addresses = [ {
          Address = "fec0::1/64";
        }];
      };
    };
  };

  services.openssh = {
    enable = true;
    settings.PermitRootLogin = "yes";
  };

  environment.systemPackages = with pkgs; [
    systemctl-tui
    tcpdump
  ];
}
