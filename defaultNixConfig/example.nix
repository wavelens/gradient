{ config, pkgs, lib, ... }:
{
  microvm = {
    # hypervisor = "cloud-hypervisor";
    vcpu = 4;
    mem = 4096;

    shares = [
      {
        tag = "ro-store";
        source = "/nix/store";
        mountPoint = "/nix/.ro-store";
      } 
    ];
    volumes = [
      {
        image = "gradient-persist.img";
        mountPoint = "/";
        size = 20 * 1024;
      }
    ];
    writableStoreOverlay = "/nix/.rw-store";

    interfaces = [
      {
        id = "eth0";
        type = "bridge";
        mac = "02:01:00:00:00:01";
        bridge = "virbr0";
      }
    ];
  };
  networking.hostName = "gradient-dev";
  users.users.root.password = "";

  networking.useDHCP = false;
  networking.nftables.enable = true;
  networking.useNetworkd = true;
  systemd.network = {
    netdevs = {
      # a bridge to connect microvms
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
          # TODO: addressConfig needs to be removed.
          # trace: warning: Using 'addressConfig' is deprecated! Move all attributes inside one level up and remove it.
          Address = "fec0::1/64"; # 
        } ];
      };
    };
  };

  services.openssh = {
    enable = true;
    # require public key authentication for better security
    settings.PasswordAuthentication = false;
    settings.KbdInteractiveAuthentication = false;
    settings.PermitRootLogin = "yes";
  };
  users.users."root" = {
    openssh.authorizedKeys.keys = [
      # Makuru
      "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIPRRdToCDUupkkwI+crB3fGDwdBIFkDsBHjOImn+qsjg openpgp:0xE8D3D833"
    ];
  };
  environment.systemPackages = with pkgs; [
    tcpdump
  ];
}
