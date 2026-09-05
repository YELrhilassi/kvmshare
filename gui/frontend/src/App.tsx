import { useEffect, useState } from "react";
import { api, type Mode } from "@/lib/bridge";
import { useRunning } from "@/lib/hooks";
import { NAV, pagesFor, type Page } from "@/lib/nav";
import { cn } from "@/lib/utils";
import HomePage from "@/pages/Home";
import ServerPage from "@/pages/Server";
import ClientPage from "@/pages/Client";
import LayoutPage from "@/pages/Layout";
import LogsPage from "@/pages/Logs";
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

  // The layout only shows pages that belong to the current role. A role
  // switch on Home can invalidate the open page — fall back to Home.
  const visible = pagesFor(mode);
  const effectivePage = visible.includes(page) ? page : "home";

  // Window title mirrors live status (also handy for taskbars/wm hints).
  useEffect(() => {
    document.title = `kvmshare — ${mode} ${running[mode] ? "running" : "stopped"}`;
  }, [running, mode]);

  const changeMode = async (m: Mode) => {
    try {
      const s = await api().GetSettings();
      await api().SetSettings({ ...s, mode: m });
      setMode(m);
    } catch {
      // The backend rejected the switch — reload what it actually has so
      // the UI can never drift from it (selection and running state stay
      // truthful even when a save fails).
      api()
        .GetSettings()
        .then((s) => setMode(s.mode))
        .catch(() => {});
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
            {NAV.filter(({ id }) => visible.includes(id)).map(({ id, label }) => (
              <button
                key={id}
                onClick={() => setPage(id)}
                className={cn(
                  "relative flex h-full items-center px-3 text-sm transition-colors",
                  effectivePage === id
                    ? "font-medium text-foreground"
                    : "text-muted-foreground hover:text-foreground",
                )}
              >
                {label}
                {effectivePage === id && (
                  <span className="absolute inset-x-3 bottom-0 h-0.5 rounded-full bg-primary" />
                )}
              </button>
            ))}
          </nav>

          <div className="ml-auto flex items-center gap-2 text-xs text-muted-foreground">
            <span
              className={cn(
                "h-2 w-2 rounded-full",
                running[mode] ? "bg-emerald-500" : "bg-muted-foreground/40",
              )}
            />
            <span className="font-medium text-foreground/80">{mode}</span>
            {running[mode] ? "running" : "stopped"}
          </div>
        </header>

        {/* Page */}
        <main className="min-h-0 flex-1 overflow-hidden">
          {effectivePage === "home" && (
            <HomePage mode={mode} onModeChange={changeMode} onNavigate={setPage} running={running} />
          )}
          {effectivePage === "server" && <ServerPage />}
          {effectivePage === "client" && <ClientPage />}
          {effectivePage === "layout" && <LayoutPage />}
          {effectivePage === "logs" && <LogsPage mode={mode} />}
        </main>
      </div>
    </TooltipProvider>
  );
}