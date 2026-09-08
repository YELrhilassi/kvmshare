import { createContext, useCallback, useContext, useEffect, useMemo, useState, type ReactNode } from "react";
import { api, onState, type ClientState, type ConnectedClient, type Mode, type Peer } from "@/lib/bridge";

export interface RunningStatus {
  server: boolean;
  client: boolean;
}

interface AppContextValue {
  mode: Mode;
  /** Persist a role switch and update the store. */
  setMode: (m: Mode) => Promise<void>;
  running: RunningStatus;
  /** The client's real connection state ("connected" / "connecting" / "disconnected"). */
  clientState: ClientState;
  /** Server-side: connected machines. Client-side: empty. */
  clients: ConnectedClient[];
  /** Every kvmshare machine seen on the network. */
  peers: Peer[];
  /** One-shot re-read after a user action (a response to a click, not polling). */
  refresh: () => Promise<void>;
}

const AppContext = createContext<AppContextValue | null>(null);

// One store for the whole app. The backend owns the live picture and
// pushes a `kvmshare:state` snapshot whenever anything changes — the
// page subscribes once and never polls. `refresh()` exists for instant
// feedback after a click; steady state is pure events.
export function AppProvider({ children }: { children: ReactNode }) {
  const [mode, setModeState] = useState<Mode>("server");
  const [running, setRunning] = useState<RunningStatus>({ server: false, client: false });
  const [clientState, setClientState] = useState<ClientState>({ status: "disconnected", server: "" });
  const [clients, setClients] = useState<ConnectedClient[]>([]);
  const [peers, setPeers] = useState<Peer[]>([]);

  const refresh = useCallback(async () => {
    try {
      const [server, client, cs] = await Promise.all([
        api().ServerRunning(),
        api().ClientRunning(),
        api().ClientStatus(),
      ]);
      setRunning({ server, client });
      setClientState(cs);
    } catch {
      /* bridge not ready yet — keep the last known state */
    }
  }, []);

  useEffect(() => {
    let alive = true;
    void api()
      .GetSettings()
      .then((s) => {
        if (alive) setModeState(s.mode);
      })
      .catch(() => {});
    // The one subscription: every snapshot replaces the store atomically.
    const off = onState((s) => {
      if (!alive) return;
      setModeState(s.mode);
      setRunning(s.running);
      setClientState(s.clientState);
      setClients(s.clients);
      setPeers(s.peers);
    });
    void refresh(); // seed before the first event arrives
    return () => {
      alive = false;
      off();
    };
  }, [refresh]);

  const setMode = useCallback(async (m: Mode) => {
    try {
      const s = await api().GetSettings();
      await api().SetSettings({ ...s, mode: m });
      setModeState(m);
    } catch {
      // The backend rejected the switch — reload what it actually has so
      // the UI can never drift from it.
      const s = await api().GetSettings().catch(() => null);
      if (s) setModeState(s.mode);
    }
  }, []);

  const value = useMemo(
    () => ({ mode, setMode, running, clientState, clients, peers, refresh }),
    [mode, setMode, running, clientState, clients, peers, refresh],
  );

  return <AppContext.Provider value={value}>{children}</AppContext.Provider>;
}

export function useApp(): AppContextValue {
  const ctx = useContext(AppContext);
  if (!ctx) throw new Error("useApp must be used inside <AppProvider>");
  return ctx;
}