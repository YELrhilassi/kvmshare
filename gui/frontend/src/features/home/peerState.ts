import type { ConnectedClient, Peer } from "@/lib/bridge";
import { DEFAULT_PORT } from "@/lib/constants";

/**
 * Live network state for one machine — the single vocabulary the Home
 * page (chips, rails, desk map) speaks. Exactly one state per peer:
 *
 * - `blocked`   — refused here, until unblocked. Outranks everything.
 * - `connected` — an active session with this machine right now.
 * - `ready`     — trusted; the session will form as soon as it is online.
 * - `nearby`    — running and discovered, but not yet trusted.
 * - `offline`   — known but not running.
 */
export type PeerState = "connected" | "blocked" | "ready" | "nearby" | "offline";

/** True when the peer's role is the one this machine can work with. */
export function peerUsable(isServer: boolean, p: Peer): boolean {
  return isServer ? p.role === "client" : p.role === "server";
}

/**
 * Same prefix contract as the backend (ids.Trusted): an entry matches a
 * machine id when either is a prefix of the other, and entries shorter
 * than 4 chars are ignored.
 */
export function matchesID(list: string[], id: string): boolean {
  return list.some((t) => {
    const entry = t.trim();
    if (entry.length < 4) return false;
    return id === entry || id.startsWith(entry) || entry.startsWith(id);
  });
}

/** `addr:port` for a peer, with the default port filled in. */
export function peerAddr(p: Peer): string {
  return `${p.addr}:${p.port || DEFAULT_PORT}`;
}

/** The full address of the server this client is attached to. */
export function clientServerAddr(clientState: { server: string }): string {
  return clientState.server || "";
}

/**
 * Classify one peer against the live picture. A machine only counts as
 * live when its role is actually running: a GUI that is open but sharing
 * nothing is not nearby (it used to linger as "nearby" forever after you
 * stopped every service on it).
 */
export function peerState(
  isServer: boolean,
  p: Peer,
  clients: ConnectedClient[],
  clientState: { status: string; server: string },
  trusted: string[],
  revoked: string[],
): PeerState {
  // A block outranks everything: the machine is refused, so its
  // trust or liveness is beside the point.
  if (matchesID(revoked, p.id)) return "blocked";
  if (isServer) {
    if (clients.some((c) => c.id === p.id)) return "connected";
  } else if (clientState.status === "connected" && clientState.server === peerAddr(p)) {
    return "connected";
  }
  if (matchesID(trusted, p.id)) return "ready";
  if (p.active) return "nearby";
  return "offline";
}
