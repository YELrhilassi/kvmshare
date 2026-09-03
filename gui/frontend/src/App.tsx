import { useEffect, useState } from "react";
import { api, type Mode } from "@/lib/bridge";
import { useRunning } from "@/lib/hooks";
import { NAV, type Page } from "@/lib/nav";
import { cn } from "@/lib/utils";
import HomePage from "@/pages/Home";
import ServerPage from "@/pages/Server";
import ClientPage from "@/pages/Client";
import LayoutPage from "@/pages/Layout";
import { TooltipProvider } from "@/components/ui/tooltip";

export default function App() {
  const [page, setPage] = useState<Page>("home");
  const [mode, setMode] = useState<Mode>("server");
  const running = useRunning();

  useEffect(() => {
    void api()
      .GetSettings()
      .then((s) => setMode(s.mode))
      .catch(() => {});
  }, []);

  // Window title mirrors live status (also handy for taskbars/wm hints).
  useEffect(() => {
    document.title = `kvmshare — server ${running.server ? "running" : "stopped"} · client ${running.client ? "running" : "stopped"}`;
  }, [running]);

  const changeMode = async (m: Mode) => {
    try {
      const s = await api().GetSettings();
      await api().SetSettings({ ...s, mode: m });
      setMode(m);
    } catch {
      /* keep current mode */
    }
  };

  return (
    <TooltipProvider delayDuration={200}>
      <div className="flex h-screen w-screen flex-col overflow-hidden bg-background text-foreground">
        {/* Top bar */}
        <header className="flex h-14 shrink-0 items-center gap-8 border-b border-border/70 px-8">
          <div className="flex items-center gap-2.5">
            <span className="flex h-6 w-6 items-center justify-center rounded-md bg-primary text-xs font-bold text-primary-foreground">
              K
            </span>
            <span className="text-sm font-semibold tracking-tight">kvmshare</span>
          </div>

          <nav className="flex h-full items-center gap-1">
            {NAV.map(({ id, label }) => (
              <button
                key={id}
                onClick={() => setPage(id)}
                className={cn(
                  "relative flex h-full items-center px-3 text-sm transition-colors",
                  page === id
                    ? "font-medium text-foreground"
                    : "text-muted-foreground hover:text-foreground",
                )}
              >
                {label}
                {page === id && (
                  <span className="absolute inset-x-3 bottom-0 h-0.5 rounded-full bg-primary" />
                )}
              </button>
            ))}
          </nav>

          <div className="ml-auto flex items-center gap-6 text-xs text-muted-foreground">
            <span className="flex items-center gap-1.5">
              <span
                className={cn(
                  "h-2 w-2 rounded-full",
                  running.server ? "bg-emerald-500" : "bg-muted-foreground/40",
                )}
              />
              server {running.server ? "running" : "stopped"}
            </span>
            <span className="flex items-center gap-1.5">
              <span
                className={cn(
                  "h-2 w-2 rounded-full",
                  running.client ? "bg-emerald-500" : "bg-muted-foreground/40",
                )}
              />
              client {running.client ? "running" : "stopped"}
            </span>
            <button
              onClick={() => setPage("home")}
              title="Change on Home"
              className="rounded-full border border-border px-2.5 py-0.5 font-medium text-foreground/80 transition-colors hover:border-primary/60 hover:text-foreground"
            >
              {mode}
            </button>
          </div>
        </header>

        {/* Page */}
        <main className="min-h-0 flex-1 overflow-hidden">
          {page === "home" && (
            <HomePage mode={mode} onModeChange={changeMode} onNavigate={setPage} running={running} />
          )}
          {page === "server" && <ServerPage />}
          {page === "client" && <ClientPage />}
          {page === "layout" && <LayoutPage />}
        </main>
      </div>
    </TooltipProvider>
  );
}