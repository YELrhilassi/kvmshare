import { useMemo } from "react";
import { useApp } from "@/app/AppProvider";
import { peerAddr, peerState, peerUsable, type PeerState } from "@/features/home/peerState";
import type { Peer } from "@/lib/bridge";
import { cn } from "@/lib/utils";

/**
 * Live control flow between machines, drawn as a small diagram.
 *
 * Direction is *meaning*, not decoration: the arrow always points from
 * the machine being controlled toward the machine in control, because
 * that is the sentence the session forms ("hp is using pc's keyboard").
 * The dot riding the link moves in that same direction, so a glance at
 * the map answers "who is driving whom" without reading a word.
 *
 * The map never lies about a session: when the client is connected but
 * discovery has not listed the server (the exact failure mode that used
 * to show "connected" above an empty peer list), the server node is
 * still drawn from the live connection state.
 *
 * Visual language follows the app: dark, flat, hairline strokes, no
 * glow. Motion is only the flow dot and a connecting pulse, both
 * suppressed under prefers-reduced-motion.
 */

const NODE_W = 150;
const NODE_H = 40;
const SELF_X = 24;
const PEER_X = 386;
/** Vertical slots for peer nodes; the self node centers on the stack. */
const SLOT_YS = [30, 88, 146];
const VIEW_W = 560;
const VIEW_H = 196;

interface NodeSpec {
  key: string;
  name: string;
  role: string;
  state: PeerState | "self-active" | "self-idle";
  /** The node's address, when it has one — how the dialing pulse
   *  matches the server the client is connecting to. */
  addr: string;
  x: number;
  y: number;
}

interface LinkSpec {
  key: string;
  /** SVG path string between the two nodes. */
  d: string;
  /** The link carries a live session. */
  active: boolean;
  /** Trusted but not yet connected: dotted "will connect" line. */
  pending: boolean;
  /** Client still dialing: pulsing dashes toward the server. */
  connecting: boolean;
}

export default function DeskMap() {
  const { mode, running, clientState, clients, peers, trusted, revoked, clientName } = useApp();
  const reducedMotion = useMemo(
    () => window.matchMedia("(prefers-reduced-motion: reduce)").matches,
    [],
  );

  const isServer = mode === "server";

  const { nodes, links } = useMemo(() => {
    const usablePeers = peers.filter((p) => peerUsable(isServer, p));

    // A client that is connected shows its server even when discovery
    // has not listed it — the live connection outranks the peer table.
    const syntheticServer: Peer | null =
      !isServer && clientState.status === "connected" && !usablePeers.some((p) => peerAddr(p) === clientState.server)
        ? { id: "", name: clientState.server || "server", role: "server", addr: "", port: 0, active: true }
        : null;
    const shown = syntheticServer ? [...usablePeers, syntheticServer] : usablePeers;

    const selfState = isServer ? (running.server ? "self-active" : "self-idle") : running.client ? "self-active" : "self-idle";
    const self: NodeSpec = {
      key: "self",
      name: clientName || "this machine",
      role: isServer ? "server" : "client",
      state: selfState,
      addr: "",
      x: SELF_X,
      y: SLOT_YS[1],
    };

    const peerNodes: NodeSpec[] = shown.slice(0, SLOT_YS.length).map((p, i) => ({
      key: p.id || p.addr || p.name,
      name: p.name || p.addr,
      role: p.role,
      state: syntheticServer && p === syntheticServer ? "connected" : peerState(isServer, p, clients, clientState, trusted, revoked),
      addr: p.addr ? peerAddr(p) : p.name,
      x: PEER_X,
      y: SLOT_YS[i],
    }));

    // The client's dialing pulse rides the link to the server it is
    // actually connecting to (matched by address; discovery usually
    // knows it, and the synthetic node covers the case it does not).
    const dialing =
      !isServer && running.client && clientState.status !== "connected" && clientState.server;

    const selfRight = { x: SELF_X + NODE_W, y: self.y + NODE_H / 2 };
    const links: LinkSpec[] = peerNodes.map((n) => {
      const peerLeft = { x: PEER_X, y: n.y + NODE_H / 2 };
      // Arrow of control: from the controlled machine toward the one in
      // control — client→server when we are the client, peer→self when
      // a client uses us. Inactive links keep the same geometry.
      const [from, to] = isServer ? [peerLeft, selfRight] : [selfRight, peerLeft];
      const mx = (from.x + to.x) / 2;
      const d = `M ${from.x} ${from.y} C ${mx} ${from.y}, ${mx} ${to.y}, ${to.x} ${to.y}`;
      return {
        key: `link-${n.key}`,
        d,
        active: n.state === "connected",
        pending: n.state === "ready",
        connecting: !!dialing && (n.addr === dialing || n.name === dialing),
      };
    });

    return { nodes: [self, ...peerNodes], links };
  }, [isServer, running, clientState, clients, peers, trusted, revoked, clientName]);

  return (
    <svg
      viewBox={`0 0 ${VIEW_W} ${VIEW_H}`}
      className="w-full"
      role="img"
      aria-label="Live map of machines and who is controlling whom"
    >
      <title>Machines and control flow</title>

      {links.map((l) => (
        <g key={l.key}>
          <path
            d={l.d}
            fill="none"
            strokeWidth={l.active ? 2 : 1.25}
            className={cn(
              "transition-[stroke,stroke-width] duration-500",
              l.active
                ? "stroke-emerald-500/70"
                : l.connecting
                  ? "stroke-amber-500/70 [stroke-dasharray:5_5] deskmap-dash"
                  : l.pending
                    ? "stroke-sky-400/45 [stroke-dasharray:2_6]"
                    : "stroke-border",
            )}
          />
          {l.active && (
            <circle r={3} className="fill-emerald-400">
              {!reducedMotion && <animateMotion dur="1.8s" repeatCount="indefinite" path={l.d} />}
            </circle>
          )}
        </g>
      ))}

      {nodes.map((n) => (
        <Node key={n.key} node={n} />
      ))}

      {nodes.length === 1 && (
        <text x={VIEW_W / 2 + 20} y={VIEW_H / 2} textAnchor="middle" className="fill-muted-foreground/50 text-[11px]">
          other machines appear here when they run
        </text>
      )}
    </svg>
  );
}

const NODE_LOOK: Record<NodeSpec["state"], { box: string; roleText: string; nameText: string }> = {
  "self-active": { box: "fill-muted/70 stroke-primary/50", roleText: "fill-primary/80", nameText: "fill-foreground" },
  "self-idle": { box: "fill-muted/40 stroke-border", roleText: "fill-muted-foreground/70", nameText: "fill-foreground/80" },
  connected: { box: "fill-emerald-500/10 stroke-emerald-500/50", roleText: "fill-emerald-500/90", nameText: "fill-foreground" },
  blocked: { box: "fill-destructive/10 stroke-destructive/40", roleText: "fill-destructive/90", nameText: "fill-foreground/70" },
  ready: { box: "fill-sky-500/10 stroke-sky-400/40", roleText: "fill-sky-400/90", nameText: "fill-foreground/80" },
  nearby: { box: "fill-amber-500/10 stroke-amber-500/40", roleText: "fill-amber-500/90", nameText: "fill-foreground/80" },
  offline: { box: "fill-muted/30 stroke-border", roleText: "fill-muted-foreground/60", nameText: "fill-muted-foreground" },
};

function Node({ node }: { node: NodeSpec }) {
  const look = NODE_LOOK[node.state];
  // SVG <text> does not wrap or truncate — clip long names ourselves so
  // a long hostname can never spill out of its box.
  const name = node.name.length > 16 ? `${node.name.slice(0, 15)}…` : node.name;
  return (
    <g className="transition-[fill] duration-500">
      <rect
        x={node.x}
        y={node.y}
        width={NODE_W}
        height={NODE_H}
        rx={8}
        strokeWidth={1.25}
        className={look.box}
      />
      <text
        x={node.x + 14}
        y={node.y + 17}
        className={cn("text-[9px] font-semibold uppercase tracking-[0.14em]", look.roleText)}
      >
        {node.role}
      </text>
      <text
        x={node.x + 14}
        y={node.y + 32}
        className={cn("text-[13px] font-medium", look.nameText)}
      >
        {name}
      </text>
    </g>
  );
}
