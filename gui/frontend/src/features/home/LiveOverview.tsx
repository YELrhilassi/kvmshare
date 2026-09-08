import { useState } from "react";
import { useApp } from "@/app/AppProvider";
import { api, copyText, type Peer } from "@/lib/bridge";
import { DEFAULT_PORT } from "@/lib/constants";
import { Button } from "@/components/ui/button";
import { Section } from "@/components/Section";
import { cn, shortID } from "@/lib/utils";

// Every machine on the network that can work with this one, in one
// list, each with its own state — a connected machine never also
// appears as a bare "nearby" entry, which used to show hp twice on the
// server. The state badge says the direction out loud: on the server
// "Connected" means "connected to this machine"; on the client it means
// "this machine is connected to it".
export default function LiveOverview() {
  const { mode, clients, peers, clientState } = useApp();
  const [err, setErr] = useState("");
  const [copiedId, setCopiedId] = useState("");

  const isServer = mode === "server";
  // Only machines that can work with this one: a server lists clients,
  // a client lists servers. A same-role machine has nothing to do with
  // this machine here.
  const rows = peers.filter((p) => (isServer ? p.role === "client" : p.role === "server"));

  const stateOf = (p: Peer): "connected" | "nearby" => {
    if (isServer) return clients.some((c) => c.id === p.id) ? "connected" : "nearby";
    const addr = `${p.addr}:${p.port || DEFAULT_PORT}`;
    return clientState.status === "connected" && clientState.server === addr ? "connected" : "nearby";
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

  const roleTag = isServer ? "client" : "server";
  const empty =
    "No other machines found yet. They show up here automatically once they're running — or connect by address from the Client page.";

  return (
    <Section title="On this network">
      {rows.length === 0 ? (
        <p className="text-sm text-muted-foreground">{empty}</p>
      ) : (
        <div className="divide-y divide-border/50">
          {rows.map((p) => {
            const state = stateOf(p);
            const addr = `${p.addr}:${p.port || DEFAULT_PORT}`;
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
                        state === "connected" ? "bg-emerald-500/10 text-emerald-500" : "bg-muted/40 text-muted-foreground",
                      )}
                    >
                      {state === "connected" ? (isServer ? "connected to you" : "in control") : "nearby"}
                    </span>
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
                    state === "connected" ? (
                      <>
                        <Button variant="outline" size="sm" onClick={() => void act(() => api().ClientCommand(p.name, "disconnect"))}>
                          Disconnect
                        </Button>
                        <Button variant="outline" size="sm" onClick={() => void act(() => api().ClientCommand(p.name, "restart"))}>
                          Restart
                        </Button>
                      </>
                    ) : (
                      <Button variant="outline" size="sm" onClick={() => void act(() => api().SendConnectRequest(p.id))}>
                        Connect here
                      </Button>
                    )
                  ) : state !== "connected" ? (
                    <Button variant="outline" size="sm" onClick={() => void act(() => api().ConnectToServer(addr))}>
                      Connect
                    </Button>
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