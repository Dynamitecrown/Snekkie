//! Device vocabulary kept separate from matching and rendering.

pub struct Device {
    pub key: &'static str,
    pub label: &'static str,
    pub commands: &'static str,
    pub details: &'static str,
    pub interfaces: &'static str,
    pub sample: &'static str,
}

const CISCO_COMMANDS: &str = "show configure terminal interface shutdown ip ipv6 address hostname enable disable exit write copy running-config startup-config vlan switchport access access-list permit deny router network description duplex speed spanning-tree channel-group line vty console login banner end reload ping traceroute clock logging snmp-server route default-gateway mode trunk encapsulation mtu version service boot system do wr conf";
const CISCO_INTERFACES: &str = r"\b(?:GigabitEthernet|Gi|FastEthernet|Fa|TenGigabitEthernet|Te|TwentyFiveGigE|Twe|FortyGigabitEthernet|Fo|HundredGigE|Hu|Ethernet|Eth|Et|Serial|Se|Loopback|Lo|Tunnel|Tu|Vlan|Vl|Port-channel|Po|Mgmt)\d+(?:[/.:]\d+)*\b";

pub const DEVICES: &[Device] = &[
    Device {
        key: "cisco_ios",
        label: "Cisco IOS / IOS XE",
        commands: CISCO_COMMANDS,
        details: "vrf neighbor redistribute passive-interface route-map prefix-list policy-map class-map access-group helper-address nat overload portfast bpduguard root primary secondary allowed native dot1q channel-protocol destination source transport input output buffered trap facility severity bandwidth lease metric distance tag track",
        interfaces: CISCO_INTERFACES,
        sample: "Switch# show running-config\r\ninterface GigabitEthernet1/0/1\r\n description Uplink to core\r\n ip address 192.0.2.1 255.255.255.0\r\n crypto ikev2 encryption aes-256\r\nrouter ospf 10\r\nGi1/0/1  up  up  0011.2233.4455\r\nSwitch# ",
    },
    Device {
        key: "cisco_nxos",
        label: "Cisco NX-OS",
        commands: CISCO_COMMANDS,
        details: "feature vpc peer-link peer-keepalive domain vrf member context nve vxlan vn-segment fabric forwarding anycast-gateway port-profile checkpoint rollback install bootflash hsrp priority preempt route-map prefix-list policy-map class-map channel-mode lacp",
        interfaces: CISCO_INTERFACES,
        sample: "Nexus# show running-config\r\nfeature vpc\r\ninterface Ethernet1/1\r\n switchport access vlan 20\r\n vpc 20\r\ninterface Vlan20\r\n ip address 192.0.2.1/24\r\nNexus# ",
    },
    Device {
        key: "cisco_asa",
        label: "Cisco ASA",
        commands: "show configure terminal interface nameif security-level ip address route access-list access-group object object-group network host subnet nat global static dynamic permit deny tunnel-group group-policy webvpn policy-map class-map service-policy inspect logging copy write running-config startup-config shutdown no enable exit end",
        details: "inside outside dmz management-only same-security-traffic inter-interface intra-interface network-object service-object extended standard destination source twice-auto after-auto inactive packet-tracer input output detailed connection timeout vpn-filter split-tunnel-policy split-tunnel-network-list dhcpd",
        interfaces: CISCO_INTERFACES,
        sample: "ciscoasa# show run\r\ninterface GigabitEthernet0/1\r\n nameif inside\r\n security-level 100\r\n ip address 192.0.2.1 255.255.255.0\r\ncrypto ikev2 policy 10\r\n encryption aes-256\r\nciscoasa# ",
    },
    Device {
        key: "juniper_junos",
        label: "Juniper Junos",
        commands: "show set delete edit top up exit configure commit rollback save load run request restart interfaces interface unit family inet inet6 address system host-name routing-options protocols security zones policies firewall filter routing-instances deactivate activate insert rename copy compare display match except count",
        details: "description vlan-id vlan-tagging flexible-vlan-tagging aggregated-ether-options ether-options ethernet-switching port-mode interface-mode members native-vlan-id static next-hop qualified-next-hop routing-instance import export policy-options policy-statement term from then accept reject discard prefix-list route-filter area authentication-key encrypted-password screen source-zone destination-zone security-zone host-inbound-traffic application applications",
        interfaces: r"\b(?:ge|xe|et|fe|so|reth)-\d+/\d+/\d+(?:\.\d+)?\b|\b(?:ae|lo|irb|vlan|st)\d+(?:\.\d+)?\b",
        sample: "admin@router# show | display set\r\nset interfaces ge-0/0/0 unit 0 family inet\r\nset interfaces ge-0/0/0 unit 0 family inet6\r\n address 2001:db8::1/64;\r\nset system services ssh\r\nset protocols ospf area 0.0.0.0\r\nge-0/0/0.0  up  up  192.0.2.1/24\r\nadmin@router# ",
    },
    Device {
        key: "aruba_aoscx",
        label: "Aruba AOS-CX",
        commands: "show configure terminal interface vlan ip ipv6 address shutdown routing router vrf access-list access-group permit deny hostname copy write memory running-config startup-config exit end ping traceroute logging snmp-server aaa user ssh https-server",
        details: "lag lacp multi-chassis vsx vsf inter-switch-link keepalive role primary secondary active-gateway active-forwarding linkup-delay vlan-access vlan-trunk allowed native tagged untagged description apply dhcp-snooping loop-protect spanning-tree bpdu-guard root-guard admin-edge qos trust dscp speed mtu",
        interfaces: r"\b\d+/\d+/\d+(?:\.\d+)?\b|\b(?:lag|vlan)\d+\b",
        sample: "CX# show running-config\r\ninterface 1/1/1\r\n no shutdown\r\n vlan access 20\r\ninterface vlan 20\r\n ip address 192.0.2.1/24\r\nssh server vrf default\r\nCX# ",
    },
    Device {
        key: "arista_eos",
        label: "Arista EOS",
        commands: CISCO_COMMANDS,
        details: "mlag domain-id peer-address peer-link local-interface reload-delay heartbeat-interval management api eapi http-commands daemon agent event-handler trigger action bash maintenance session commit abort cvx vxlan vni source-interface virtual-router virtual mac-address neighbor route-map prefix-list maximum-routes bfd",
        interfaces: CISCO_INTERFACES,
        sample: "leaf# show running-config\r\ninterface Ethernet1\r\n switchport access vlan 20\r\ninterface Vlan20\r\n ip address 192.0.2.1/24\r\nrouter bgp 65001\r\n neighbor 192.0.2.2 remote-as 65002\r\nleaf# ",
    },
    Device {
        key: "fortinet_fortios",
        label: "Fortinet FortiOS",
        commands: "config edit set unset next end show get diagnose execute system interface router firewall policy address addrgrp service custom group vip ippool vpn ipsec phase1-interface phase2-interface ssl user admin vdom global routing-table static ospf bgp",
        details: "allowaccess srcintf dstintf srcaddr dstaddr action accept deny schedule always service nat status enable disable logtraffic all policyid vd name type subnet gateway device distance priority dst internet-service application-list ips-sensor webfilter-profile av-profile ssl-ssh-profile utm-status central-snat-map virtual-wan-link sdwan members health-check",
        interfaces: r"\b(?:port\d+|wan\d*|lan\d*|dmz\d*|ssl\.root|fortilink)\b",
        sample: "FGT (root) # show\r\nconfig system interface\r\n edit \"port1\"\r\n  set ip 192.0.2.1 255.255.255.0\r\n  set allowaccess ping https ssh\r\n  set proposal aes256-sha256\r\n next\r\nend",
    },
    Device {
        key: "paloalto_panos",
        label: "Palo Alto PAN-OS",
        commands: "show set delete edit top up exit configure commit revert save load run request test debug ping traceroute network interface ethernet layer2 layer3 deviceconfig system rulebase security nat rules application service objects address address-group zones zone vsys",
        details: "virtual-router virtual-wire tunnel loopback vlan aggregate-ethernet routing-table routing-protocol static-route destination nexthop ip-address ip-netmask ip-range fqdn from to source destination source-user category application-default action allow deny drop log-start log-end profile-setting profiles default-route metric admin-dist monitor ike-gateway ipsec-tunnel crypto-profiles",
        interfaces: r"\bethernet\d+/\d+(?:\.\d+)?\b|\b(?:ae\d+|tunnel\.\d+|loopback\.\d+|vlan\.\d+)\b",
        sample: "admin@PA> show system info\r\nhostname: PA\r\nip-address: 192.0.2.1\r\nipv6-link-local-address: fe80::1/64\r\nmac-address: 00:11:22:33:44:55\r\nadmin@PA# set network interface\r\n ethernet1/1 layer3 ip 192.0.2.1/24\r\nadmin@PA# ",
    },
];
