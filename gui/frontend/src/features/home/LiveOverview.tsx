import { useState } from "react";
import { useApp } from "@/app/AppProvider";
import { api, copyText, type Peer } from "@/lib/bridge";
import { DEFAULT_PORT } from "@/lib/constants";
import { Button } from "@/components/ui/button";
import { Section } from "@/components/Section";
import { cn, shortID } from "@/lib/utils";
import { RotateCw } from "lucide-react";

// Every machine on the network that can work with this one, in one list.
// A machine only counts as live when its role is actually running: a GUI
// that is open but sharing nothing is not nearby (it used to linger as
// "nearby" forever after you stopped every service on it).
//
// One decision per machine, in plain words:
//   Connect   — ask it to work with this machine (a running peer) or wait
//               for a trusted one to come online. Connecting implies
//               trust — there is no separate verb to learn.
//   Disconnect — end the session that is running right now.
//   Block     — refuse this machine until you unblock it: it can never
//               connect, and a live session ends immediately.
//   Unblock   — lift the block. (Behind the scenes Block/Unblock manage
//               both the trusted and the refused lists; the Home page
//               never makes the user think about the difference. Fine
//               control by machine id stays on the Server page.)
type RowState = "connected" | "blocked" | "ready" | "nearby" | "offline";

// Same prefix contract as the backend (ids.Trusted): an entry matches a
// machine id when either is a prefix of the other, and entries shorter
// than 4 chars are ignored.
function matchesID(list: string[], id: string): boolean {
  return list.some((t) => {
    const entry = t.trim();
    if (entry.length < 4) return false;
    return id === entry || id.startsWith(entry) || entry.startsWith(id);
  });
}

export default function LiveOverview() {
  const { mode, clients, peers, trusted, revoked, clientState, refresh } = useApp();
  const [err, setErr] = useState("");
  const [copiedId, setCopiedId] = useState("");
  const [sweeping, setSweeping] = useState(false);

  const isServer = mode === "server";
  // Every discovered machine stays listed. A strict role filter made
  // rows vanish whenever either machine changed role — which read as
  // the whole list flapping. Rows that can work with this machine (a
  // server lists clients, a client lists servers) get actions; a
  // same-role machine is still shown, muted, with its real role.
  const usable = (p: Peer) => (isServer ? p.role === "client" : p.role === "server");

  const stateOf = (p: Peer): RowState => {
    // A block outranks everything: the machine is refused, so its
    // trust or liveness is beside the point.
    if (matchesID(revoked, p.id)) return "blocked";
    if (isServer) {
      if (clients.some((c) => c.id === p.id)) return "connected";
    } else {
      const addr = `${p.addr}:${p.port || DEFAULT_PORT}`;
      if (clientState.status === "connected" && clientState.server === addr) return "connected";
    }
    if (matchesID(trusted, p.id)) return "ready";
    if (p.active) return "nearby";
    return "offline";
  };

  const act = async (fn: () => Promise<unknown>) => {
    setErr("");
    try {
      await fn();
      await refresh();
    } catch (e) {
      setErr(String(e));
    }
  };

  const copy = async (id: string) => {
    await copyText(id).catch(() => {});
    setCopiedId(id);
    window.setTimeout(() => setCopiedId(""), 1500);
  };

  // Force a discovery sweep: clear the peer map, re-announce, re-probe
  // the subnet now. The result also seeds the pushed state (the event
  // dedupe compares snapshots, so identical lists stay quiet). The
  // brief spin keeps the button honest about what it just did.
  const rescan = async () => {
    if (sweeping) return;
    setSweeping(true);
    setErr("");
    try {
      await api().RefreshDiscovery();
      await refresh();
    } catch (e) {
      setErr(String(e));
    } finally {
      window.setTimeout(() => setSweeping(false), 600);
    }
  };

  // Block is one action that refuses the machine everywhere: it leaves
  // the trusted list and joins the refused list (the backend refuses a
  // revoked machine even when trusted, so this is belt and braces).
  // Unblock only lifts the refusal; whether the machine is still
  // trusted is the Server page's business.
  const setBlocked = (p: Peer, blocked: boolean) => {
    if (blocked) {
      return act(async () => {
        await (isServer ? api().RevokeClient(p.id, true) : api().RevokeServer(p.id, true));
        await (isServer ? api().TrustClient(p.id, false) : api().TrustServer(p.id, false));
      });
    }
    return act(() => (isServer ? api().RevokeClient(p.id, false) : api().RevokeServer(p.id, false)));
  };

  const connect = (p: Peer) =>
    act(() => (isServer ? api().SendConnectRequest(p.id) : api().ConnectToServer(`${p.addr}:${p.port || DEFAULT_PORT}`)));

  // Chip label and tint per state — the row's single source of truth.
  const chip: Record<RowState, { label: string; className: string }> = {
    connected: { label: isServer ? "connected" : "in control", className: "bg-emerald-500/10 text-emerald-500" },
    blocked: { label: "blocked", className: "bg-destructive/10 text-destructive" },
    ready: { label: "ready", className: "bg-sky-500/10 text-sky-400" },
    nearby: { label: "nearby", className: "bg-amber-500/10 text-amber-500" },
    offline: { label: "offline", className: "bg-muted/40 text-muted-foreground" },
  };

  // One-line hint only where the chip alone leaves a question open.
  const hintFor = (state: RowState): string | undefined => {
    switch (state) {
      case "blocked":
        return "refused until you unblock it";
      case "ready":
        return "waiting for it to come online";
      case "offline":
        return "not running";
      default:
        return undefined;
    }
  };

  // The empty message only covers "nothing on the network at all".
  const empty =
    "No other machines found yet. They show up here automatically once they're running — or connect by address from the Client page.";

  return (
    <Section
      title="On this network"
      action={
        <Button
          variant="ghost"
          size="sm"
          className="h-7 gap-1.5 px-2 text-xs text-muted-foreground"
          onClick={() => void rescan()}
          disabled={sweeping}
          title="Clear discovered machines and probe the network again"
        >
          <RotateCw className={cn("h-3.5 w-3.5", sweeping && "animate-spin")} />
          Refresh
        </Button>
      }
    >
      {peers.length === 0 ? (
        <p className="text-sm text-muted-foreground">{empty}</p>
      ) : (
        <div className="divide-y divide-border/50">
          {peers.map((p) => {
            const state = stateOf(p);
            const addr = `${p.addr}:${p.port || DEFAULT_PORT}`;
            const c = chip[state];
            return (
              <div key={p.id} className="flex flex-wrap items-center justify-between gap-x-4 gap-y-2 py-3">
                <div className="min-w-0">
                  <div className="flex items-center gap-2">
                    <span className="text-sm font-medium">{p.name}</span>
                    <span className="rounded-full border border-border/60 px-2 py-0.5 text-[10px] uppercase tracking-wide text-muted-foreground/70">
                      {p.role}
                    </span>
                    <span className={cn("rounded-full px-2 py-0.5 text-[10px] font-medium uppercase tracking-wide", c.className)}>
                      {c.label}
                    </span>
                    {hintFor(state) && <span className="text-[10px] text-muted-foreground/50">{hintFor(state)}</span>}
                  </div>
                  <div className="mt-1 flex flex-wrap items-center gap-x-3 gap-y-1 font-mono text-[11px] text-muted-foreground/60">
                    <span>{addr}</span>
                    <span className="flex items-center gap-1">
                      id {shortID(p.id)}
                      <button
                        onClick={() => void copy(p.id)}
                        title="Copy full id"
                        className="rounded border border-border/60 px-1.5 text-[10px] text-muted-foreground transition-colors hover:bg-muted/50 hover:text-foreground"
                      >
                        {copiedId === p.id ? "copied" : "copy"}
                      </button>
                    </span>
                  </div>
                </div>
                <div className="flex shrink-0 items-center gap-1.5">
                  {!usable(p) ? (
                    <span className="text-[10px] text-muted-foreground/50">same role as this machine</span>
                  ) : state === "connected" ? (
                    isServer && (
                      <>
                        <Button variant="outline" size="sm" onClick={() => void act(() => api().ClientCommand(p.name, "disconnect"))}>
                          Disconnect
                        </Button>
                        <Button variant="outline" size="sm" onClick={() => void act(() => api().ClientCommand(p.name, "restart"))}>
                          Restart
                        </Button>
                      </>
                    )
                  ) : state === "blocked" ? (
                    <Button variant="outline" size="sm" onClick={() => void setBlocked(p, false)}>
                      Unblock
                    </Button>
                  ) : (
                    <>
                      <Button variant="outline" size="sm" onClick={() => void connect(p)}>
                        Connect
                      </Button>
                      <Button
                        variant="ghost"
                        size="sm"
                        className="text-muted-foreground hover:text-destructive"
                        onClick={() => void setBlocked(p, true)}
                        title="Refuse this machine until you unblock it"
                      >
                        Block
                      </Button>
                    </>
                  )}
                </div>
              </div>
            );
          })}
        </div>
      )}
      {err && <p className="mt-2 text-xs text-destructive">{err}</p>}
    </Section>
  );
}
