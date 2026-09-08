import { createContext, useCallback, useContext, useEffect, useMemo, useState, type ReactNode } from "react";
import { api, type ClientState, type Mode } from "@/lib/bridge";

export interface RunningStatus {
  server: boolean;
  client: boolean;
}

interface AppContextValue {
  mode: Mode;
  /** Persist a role switch (stops the running process) and update the store. */
  setMode: (m: Mode) => Promise<void>;
  running: RunningStatus;
  /** The client's real connection state ("connected" / "connecting" / "disconnected"). */
  clientState: ClientState;
  /** Re-check process state immediately (after a start/stop from Home). */
  refresh: () => Promise<void>;
}

const AppContext = createContext<AppContextValue | null>(null);

// One store for the whole app: the role and the live process state. A
// single 2s poller lives here, so pages never run their own intervals
// and can't disagree about what is running.
export function AppProvider({ children }: { children: ReactNode }) {
  const [mode, setModeState] = useState<Mode>("server");
  const [running, setRunning] = useState<RunningStatus>({ server: false, client: false });
  const [clientState, setClientState] = useState<ClientState>({ status: "disconnected", server: "" });

  const refresh = useCallback(async () => {
    try {
      const [server, client] = await Promise.all([api().ServerRunning(), api().ClientRunning()]);
      setRunning({ server, client });
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
    const tick = async () => {
      try {
        const [server, client, cs] = await Promise.all([
          api().ServerRunning(),
          api().ClientRunning(),
          api().ClientStatus(),
        ]);
        if (alive) {
          setRunning({ server, client });
          setClientState(cs);
        }
      } catch {
        /* bridge not ready yet */
      }
    };
    void tick();
    const id = setInterval(tick, 2000);
    return () => {
      alive = false;
      clearInterval(id);
    };
  }, []);

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
    () => ({ mode, setMode, running, clientState, refresh }),
    [mode, setMode, running, clientState, refresh],
  );

  return <AppContext.Provider value={value}>{children}</AppContext.Provider>;
}

export function useApp(): AppContextValue {
  const ctx = useContext(AppContext);
  if (!ctx) throw new Error("useApp must be used inside <AppProvider>");
  return ctx;
}