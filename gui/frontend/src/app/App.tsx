import { useEffect, useState } from "react";
import { TooltipProvider } from "@/components/ui/tooltip";
import { cn } from "@/lib/utils";
import { AppProvider, useApp } from "@/app/AppProvider";
import { NAV, pagesFor, type Page } from "@/app/nav";
import HomePage from "@/features/home/HomePage";
import ServerPage from "@/features/server/ServerPage";
import ClientPage from "@/features/client/ClientPage";
import LayoutPage from "@/features/layout/LayoutPage";
import LogsPage from "@/features/logs/LogsPage";

// Plain-language label for the top-right status: what is happening right
// now, not what the process is called. For the client this is the real
// connection state (a running process that is not connected says
// "Connecting…", never "Connected").
function statusLabel(mode: "server" | "client", running: boolean, connected: boolean): string {
  if (mode === "server") return running ? "Sharing" : "Stopped";
  if (connected) return "Connected";
  if (running) return "Connecting…";
  return "Stopped";
}

function Shell() {
  const { mode, running, clientState } = useApp();
  const [page, setPage] = useState<Page>("home");

  // The layout only shows pages that belong to the current role. A role
  // switch on Home can invalidate the open page — fall back to Home.
  const visible = pagesFor(mode);
  const effectivePage = visible.includes(page) ? page : "home";

  // Window title mirrors live status (also handy for taskbars/wm hints).
  useEffect(() => {
    document.title = `kvmshare — ${statusLabel(mode, running[mode], clientState.status === "connected")}`;
  }, [running, mode, clientState.status]);

  return (
    <TooltipProvider delayDuration={200}>
      <div className="flex h-screen w-screen flex-col overflow-hidden bg-background text-foreground">
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

          <div className="ml-auto flex items-center gap-2 text-xs">
            <span
              className={cn(
                "h-2 w-2 rounded-full",
                mode === "server" ? (running.server ? "bg-emerald-500" : "bg-muted-foreground/40") : clientState.status === "connected" ? "bg-emerald-500" : clientState.status === "connecting" ? "bg-amber-500" : "bg-muted-foreground/40",
              )}
            />
            <span className="font-medium text-foreground/80">
              {statusLabel(mode, running[mode], clientState.status === "connected")}
            </span>
          </div>
        </header>

        <main className="min-h-0 flex-1 overflow-hidden">
          {effectivePage === "home" && <HomePage onNavigate={setPage} />}
          {effectivePage === "server" && <ServerPage />}
          {effectivePage === "client" && <ClientPage />}
          {effectivePage === "layout" && <LayoutPage />}
          {effectivePage === "logs" && <LogsPage />}
        </main>
      </div>
    </TooltipProvider>
  );
}

export default function App() {
  return (
    <AppProvider>
      <Shell />
    </AppProvider>
  );
}