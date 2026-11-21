/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ lib, bridge, bridgeAddress, guestDnsForwarderEnabled, allowedPrivateNetworks, guestConnectableHostTcpPorts, execGuestHasIpv6Address, blockedGlobalIPv6Networks }: let
  guestBlockedDestinationIpv4Cidrs = [
    "0.0.0.0/8"
    "10.0.0.0/8"
    "100.64.0.0/10"
    "127.0.0.0/8"
    "169.254.0.0/16"
    "172.16.0.0/12"
    "192.168.0.0/16"
    "224.0.0.0/4"
    "240.0.0.0/4"
  ];
  guestBlockedDestinationIpv6Cidrs = [
    "2001::/23"
    "2001:db8::/32"
    "2002::/16"
    "3fff::/20"
  ];
  allowedPrivateIpv4NetworkCidrs = lib.filter (network: !lib.hasInfix ":" network) allowedPrivateNetworks;
  allowedPrivateNetworksIpv6Entries = lib.filter (lib.hasInfix ":") allowedPrivateNetworks;
  renderNftablesAnonymousSetText = elements: "{ ${lib.concatStringsSep ", " elements} }";
  render_nftables_rule_lines_with_fixed_indentation = lib.concatMapStrings (line: "\n    ${line}");
  bridgeTablePreroutingIpv6GuestTapNftablesRuleLines = lib.optionals execGuestHasIpv6Address [
    "ether type ip6 icmpv6 type { nd-neighbor-advert, nd-router-solicit, nd-router-advert, nd-redirect, mld-listener-query, mld-listener-report, mld-listener-done, mld2-listener-report } drop"
    "ether type ip6 udp sport 547 drop"
    "ether type ip6 iifname . ether saddr . ip6 saddr @guest-tap-ipv6-addresses accept"
  ];
  ipv6_guest_forward_chain_nftables_rule_lines =
    if execGuestHasIpv6Address then
      lib.optional (blockedGlobalIPv6Networks != [ ]) "ip6 daddr ${renderNftablesAnonymousSetText blockedGlobalIPv6Networks} drop"
      ++ lib.optional (allowedPrivateNetworksIpv6Entries != [ ]) "ip6 daddr ${renderNftablesAnonymousSetText allowedPrivateNetworksIpv6Entries} accept"
      ++ [
        "ip6 daddr != 2000::/3 drop"
        "ip6 daddr ${renderNftablesAnonymousSetText guestBlockedDestinationIpv6Cidrs} drop"
      ]
    else [ "meta nfproto ipv6 drop" ];
in {
  # The first part can create the bridge table, its set and chain when absent. This step can also empty the chain. Set elements survive a reload, so already running guests keep network. The inet table can hold no state. A reload can replace the inet table as a whole.
  atomicNftablesTableReloadScriptKeepingGuestAddressSet = ''
    table bridge gradient-guests {
      set guest-tap-addresses { type ifname . ether_addr . ipv4_addr; }${lib.optionalString execGuestHasIpv6Address "\n  set guest-tap-ipv6-addresses { type ifname . ether_addr . ipv6_addr; }"}
      chain drop-guest-tap-source-mismatch { type filter hook prerouting priority filter; policy accept; }
    }
    flush chain bridge gradient-guests drop-guest-tap-source-mismatch
    table bridge gradient-guests {
      chain drop-guest-tap-source-mismatch {
        type filter hook prerouting priority filter; policy accept;
        iifname != "gr-tap-*" accept
        ether type ip iifname . ether saddr . ip saddr @guest-tap-addresses accept
        ether type arp iifname . ether saddr . arp saddr ip @guest-tap-addresses iifname . arp saddr ether . arp saddr ip @guest-tap-addresses accept${render_nftables_rule_lines_with_fixed_indentation bridgeTablePreroutingIpv6GuestTapNftablesRuleLines}
        drop
      }
    }

    table inet gradient-guests {}
    delete table inet gradient-guests
    table inet gradient-guests {
      chain guest-forward-drop-private-and-link-local {
        type filter hook forward priority filter; policy accept;
        iifname != "${bridge}" accept
        oifname "${bridge}" drop
        ct direction reply ct state established accept
        ${lib.optionalString (allowedPrivateIpv4NetworkCidrs != [ ]) "ip daddr ${renderNftablesAnonymousSetText allowedPrivateIpv4NetworkCidrs} accept"}
        ip daddr ${renderNftablesAnonymousSetText guestBlockedDestinationIpv4Cidrs} drop${render_nftables_rule_lines_with_fixed_indentation ipv6_guest_forward_chain_nftables_rule_lines}
      }

      chain guest-to-host-input {
        type filter hook input priority filter; policy accept;
        iifname != "${bridge}" accept
        ct direction reply ct state established,related accept${render_nftables_rule_lines_with_fixed_indentation (lib.optional execGuestHasIpv6Address "icmpv6 type nd-neighbor-solicit accept")}
        ${lib.optionalString guestDnsForwarderEnabled "ip daddr ${bridgeAddress} meta l4proto { tcp, udp } th dport 53 accept"}
        ${lib.optionalString (guestConnectableHostTcpPorts != [ ]) "tcp dport ${renderNftablesAnonymousSetText (map toString guestConnectableHostTcpPorts)} accept"}
        drop
      }
    }
  '';

  # The script does hold an exclusive flock on the record directory during the table load and the
  # write-back of records. The guest supervisor does take the same lock to change a set element and its
  # record. So a guest that stops during a reload cannot leave a set element without a record. The
  # script can skip records of tap devices that are gone. The next guest on that tap does write a new
  # record.
  reloadGuestNftablesScriptTextKeepingAddressesOfRunningGuests = { guestTapAddressRecordDirectory, guestNetworkFilterNftablesScriptFilePath }: ''
    set -euo pipefail
    mkdir -p -m 0700 "${guestTapAddressRecordDirectory}"
    exec {record_directory_lock_file_descriptor}<"${guestTapAddressRecordDirectory}"
    flock --exclusive "$record_directory_lock_file_descriptor"
    nft -f "${guestNetworkFilterNftablesScriptFilePath}"
    for record in "${guestTapAddressRecordDirectory}"/gr-tap-*; do
      name=''${record##*/}
      [[ $name =~ ^gr-tap-[0-9]+$ ]] || continue
      ip link show dev "$name" > /dev/null 2>&1 || continue
      mapfile -t tap_address_record_lines < "$record"
      if (( ''${#tap_address_record_lines[@]} != 3 )); then
        echo "Skipped record $record: it doesn't contain 3 lines" >&2
        continue
      fi
      nft add element bridge gradient-guests guest-tap-addresses "''${tap_address_record_lines[0]}" || echo "Failed to add record $record for the guest address to set guest-tap-addresses" >&2
      [[ -z ''${tap_address_record_lines[1]} ]] || nft add element bridge gradient-guests guest-tap-ipv6-addresses "''${tap_address_record_lines[1]}" || echo "Failed to add record $record for the guest IPv6 address to set guest-tap-ipv6-addresses" >&2
    done
  '';
}
