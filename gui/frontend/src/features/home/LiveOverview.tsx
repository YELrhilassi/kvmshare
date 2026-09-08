import { useState } from "react";
import { useApp } from "@/app/AppProvider";
import { api, copyText, type Peer } from "@/lib/bridge";
import { DEFAULT_PORT } from "@/lib/constants";
import { Button } from "@/components/ui/button";
import { Section } from "@/components/Section";
import { cn, shortID } from "@/lib/utils";

// Every machine on the network that can work with this one, in one
// list, each with its own state. A machine only counts as "nearby"
// when its role is actually running: a GUI that is open but sharing
// nothing is not a live machine (it used to linger as "nearby" forever
// after you stopped every service on it). An idle machine stays
// visible as "idle" (or "trusted" when trusted) so it can still be
// trusted and asked to connect — hiding it entirely would make it
// impossible to ever set up a first connection.
//
// Trust lives here, next to the machine it concerns: trust a machine
// from its row, revoke it the same way.
type RowState = "connected" | "nearby" | "trusted" | "idle";

// Same prefix contract as the backend (idTrusted): a trusted entry
// matches a peer when either is a prefix of the other, and entries
// shorter than 4 chars are ignored.
function isTrusted(trusted: string[], id: string): boolean {
  return trusted.some((t) => {
    const entry = t.trim();
    if (entry.length < 4) return false;
    return id === entry || id.startsWith(entry) || entry.startsWith(id);
  });
}

export default function LiveOverview() {
  const { mode, clients, peers, trusted, clientState } = useApp();
  const [err, setErr] = useState("");
  const [copiedId, setCopiedId] = useState("");

  const isServer = mode === "server";
  // Only machines that can work with this one: a server lists clients,
  // a client lists servers. A same-role machine has nothing to do with
  // this machine here.
  const rows = peers.filter((p) => (isServer ? p.role === "client" : p.role === "server"));

  const stateOf = (p: Peer): RowState => {
    if (isServer) {
      if (clients.some((c) => c.id === p.id)) return "connected";
    } else {
      const addr = `${p.addr}:${p.port || DEFAULT_PORT}`;
      if (clientState.status === "connected" && clientState.server === addr) return "connected";
    }
    if (!p.active) return isTrusted(trusted, p.id) ? "trusted" : "idle";
    return "nearby";
  };

  const act = async (fn: () => Promise<unknown>) => {
    setErr("");
    try {
      await fn();
    } catch (e) {
      setErr(String(e));
    }
  };

  const copy = async (id: string) => {
    await copyText(id).catch(() => {});
    setCopiedId(id);
    window.setTimeout(() => setCopiedId(""), 1500);
  };

  const trust = (p: Peer, on: boolean) =>
    act(() => (isServer ? (on ? api().TrustClient(p.id) : api().RevokeClient(p.id)) : on ? api().TrustServer(p.id) : api().RevokeServer(p.id)));

  const roleTag = isServer ? "client" : "server";
  // Every discovered machine is actionable (trust, connect); there is
  // nothing to filter out. The empty message only covers "nothing on
  // the network at all".
  const visible = rows;
  const empty =
    "No other machines found yet. They show up here automatically once they're running — or connect by address from the Client page.";

  return (
    <Section title="On this network">
      {visible.length === 0 ? (
        <p className="text-sm text-muted-foreground">{empty}</p>
      ) : (
        <div className="divide-y divide-border/50">
          {visible.map((p) => {
            const state = stateOf(p);
            const addr = `${p.addr}:${p.port || DEFAULT_PORT}`;
            const connected = state === "connected";
            const trustedPeer = isTrusted(trusted, p.id);
            return (
              <div key={p.id} className="flex flex-wrap items-center justify-between gap-x-4 gap-y-2 py-3">
                <div className="min-w-0">
                  <div className="flex items-center gap-2">
                    <span className="text-sm font-medium">{p.name}</span>
                    <span className="rounded-full border border-border/60 px-2 py-0.5 text-[10px] uppercase tracking-wide text-muted-foreground/70">
                      {roleTag}
                    </span>
                    <span
                      className={cn(
                        "rounded-full px-2 py-0.5 text-[10px] font-medium uppercase tracking-wide",
                        connected
                          ? "bg-emerald-500/10 text-emerald-500"
                          : state === "trusted"
                            ? "bg-sky-500/10 text-sky-400"
                            : state === "nearby"
                              ? "bg-amber-500/10 text-amber-500"
                              : "bg-muted/40 text-muted-foreground",
                      )}
                    >
                      {connected
                        ? isServer
                          ? "connected to you"
                          : "in control"
                        : state === "trusted"
                          ? "trusted"
                          : state === "nearby"
                            ? "nearby"
                            : "idle"}
                    </span>
                    {state === "trusted" && (
                      <span className="text-[10px] text-muted-foreground/50">not running — ask it to connect</span>
                    )}
                    {state === "idle" && <span className="text-[10px] text-muted-foreground/50">not running</span>}
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
                  {isServer ? (
                    <>
                      {connected ? (
                        <>
                          <Button variant="outline" size="sm" onClick={() => void act(() => api().ClientCommand(p.name, "disconnect"))}>
                            Disconnect
                          </Button>
                          <Button variant="outline" size="sm" onClick={() => void act(() => api().ClientCommand(p.name, "restart"))}>
                            Restart
                          </Button>
                          <Button variant="ghost" size="sm" className="text-muted-foreground" onClick={() => void trust(p, false)}>
                            Revoke
                          </Button>
                        </>
                      ) : (
                        <>
                          <Button variant="ghost" size="sm" className="text-muted-foreground" onClick={() => void trust(p, !trustedPeer)}>
                            {trustedPeer ? "Revoke" : "Trust"}
                          </Button>
                          <Button variant="outline" size="sm" onClick={() => void act(() => api().SendConnectRequest(p.id))}>
                            Connect here
                          </Button>
                        </>
                      )}
                    </>
                  ) : state === "trusted" ? (
                    <Button variant="ghost" size="sm" className="text-muted-foreground" onClick={() => void trust(p, false)}>
                      Revoke
                    </Button>
                  ) : !connected ? (
                    <>
                      <Button variant="ghost" size="sm" className="text-muted-foreground" onClick={() => void trust(p, !trustedPeer)}>
                        {trustedPeer ? "Revoke" : "Trust"}
                      </Button>
                      <Button variant="outline" size="sm" onClick={() => void act(() => api().ConnectToServer(addr))}>
                        Connect
                      </Button>
                    </>
                  ) : null}
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