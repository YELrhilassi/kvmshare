import type { InterfaceInfo } from "@/lib/bridge";

// Interface names that never carry a peer-reachable address: docker
// bridges, veth pairs, virbr/libvirt, tailscale/tun/wg tunnels, and the
// loopback. Advertising these (e.g. 172.17.0.1) tells other machines to
// connect to an address that only exists on this box.
const VIRTUAL_IFACE = /^(docker|veth|br-|virbr|tailscale|tun|wg|lo|utun)/i;

// The addresses other machines can reach this one at: IPv4, LAN ranges
// first, loopback/link-local and virtual interfaces excluded. At most
// two, so the Home page stays clean on machines with several NICs —
// there is no point in listing every secondary address.
export function lanAddresses(ifaces: InterfaceInfo[]): string[] {
  const v4: string[] = [];
  for (const ifc of ifaces) {
    if (VIRTUAL_IFACE.test(ifc.name)) continue;
    for (const addr of ifc.addrs) {
      if (
        /^\d+\.\d+\.\d+\.\d+$/.test(addr) &&
        !addr.startsWith("127.") &&
        !addr.startsWith("169.254.")
      ) {
        v4.push(addr);
      }
    }
  }
  const score = (ip: string) =>
    ip.startsWith("192.168.") ? 0 : ip.startsWith("10.") ? 1 : ip.startsWith("172.") ? 2 : 3;
  v4.sort((a, b) => score(a) - score(b));
  return v4.slice(0, 2);
}