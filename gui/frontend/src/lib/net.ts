import type { InterfaceInfo } from "@/lib/bridge";

// The addresses other machines can reach this one at: IPv4, LAN ranges
// first, loopback and link-local excluded.
export function lanAddresses(ifaces: InterfaceInfo[]): string[] {
  const v4: string[] = [];
  for (const ifc of ifaces) {
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
  return v4.slice(0, 3);
}