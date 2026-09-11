import { useCallback, useEffect, useState } from "react";
import { api, type LayoutConfig, type Screen } from "@/lib/bridge";
import Toolbar from "@/features/layout/Toolbar";
import Canvas from "@/features/layout/canvas/Canvas";
import ScreenInspector from "@/features/layout/inspector/ScreenInspector";
import InputShortcutsPage from "@/features/layout/InputShortcutsPage";
import { useLayoutDocument } from "@/features/layout/useLayoutDocument";
import { useCanvasView } from "@/features/layout/useCanvasView";
import { PageSkeleton } from "@/components/PageSkeleton";
import { cn } from "@/lib/utils";

// Layout page tabs: Arrangement is the canvas (where screens sit);
// Input & shortcuts is the behavior sub-page (what keys and pointers
// do). Same config document behind both, one Save per tab area.
type Tab = "arrange" | "input";

function TabBar({ tab, onTab }: { tab: Tab; onTab: (t: Tab) => void }) {
  const tabs: { id: Tab; label: string }[] = [
    { id: "arrange", label: "Arrangement" },
    { id: "input", label: "Input & shortcuts" },
  ];
  return (
    <div className="flex items-center gap-1 border-b border-border/60 px-8">
      {tabs.map((t) => (
        <button
          key={t.id}
          onClick={() => onTab(t.id)}
          className={cn(
            "-mb-px border-b-2 px-3 py-2.5 text-sm transition-colors",
            tab === t.id
              ? "border-primary text-foreground"
              : "border-transparent text-muted-foreground hover:text-foreground",
          )}
        >
          {t.label}
        </button>
      ))}
    </div>
  );
}

export default function LayoutPage() {
  const [config, setConfig] = useState<LayoutConfig | null>(null);
  const [error, setError] = useState("");
  const [tab, setTab] = useState<Tab>("arrange");

  useEffect(() => {
    let alive = true;
    void api()
      .LoadConfig()
      .then((c) => {
        if (alive) setConfig(c);
      })
      .catch((e) => {
        if (alive) setError(String(e));
      });
    return () => {
      alive = false;
    };
  }, []);

  if (error) {
    return (
      <div className="flex h-full items-center justify-center p-6">
        <p className="max-w-md text-sm text-destructive">{error}</p>
      </div>
    );
  }

  if (!config) {
    return <PageSkeleton rows={2} />;
  }

  return (
    <div className="flex h-full flex-col">
      <TabBar tab={tab} onTab={setTab} />
      {tab === "arrange" ? <Editor config={config} /> : <InputShortcutsPage />}
    </div>
  );
}

// The editor: one reducer for the document, one hook for the view, and
// three presentational pieces (toolbar / canvas / inspector). The full
// config is passed in so a save round-trips every field (port, network
// policy, …) — the layout page only ever edits screens.
function Editor({ config }: { config: LayoutConfig }) {
  const { state, dispatch } = useLayoutDocument(config.screens);
  const view = useCanvasView(state.screens);
  const { screens, selected, lock, snap, dirty, savedMsg, error } = state;

  // Layout validation, live: overlaps and degenerate sizes surface here
  // while the user drags, so a broken arrangement never has to be
  // discovered at the screen. Mirrors the server-side Layout::issues.
  const issues: string[] = (() => {
    const out: string[] = [];
    for (let i = 0; i < screens.length; i++) {
      const a = screens[i];
      if (a.width <= 0 || a.height <= 0) {
        out.push(`"${a.name || "screen"}" has no size — give it its real resolution`);
        continue;
      }
      for (let j = i + 1; j < screens.length; j++) {
        const b = screens[j];
        if (a.x < b.x + b.width && b.x < a.x + a.width && a.y < b.y + b.height && b.y < a.y + a.height) {
          out.push(`"${a.name || "screen"}" and "${b.name || "screen"}" overlap — move them apart`);
        }
      }
    }
    return out;
  })();

  // A "saved" toast is only ever transient.
  useEffect(() => {
    if (!savedMsg) return;
    const t = window.setTimeout(() => dispatch({ type: "clearMessage" }), 3000);
    return () => window.clearTimeout(t);
  }, [savedMsg, dispatch]);

  // dispatch is stable, so these callbacks keep a constant identity and
  // the memoized canvas nodes only re-render when their own data changes.
  const onSelect = useCallback((index: number) => dispatch({ type: "select", index }), [dispatch]);
  const onMove = useCallback(
    (index: number, x: number, y: number) => dispatch({ type: "move", index, x, y }),
    [dispatch],
  );
  const onPatch = useCallback(
    (index: number, patch: Partial<Screen>) => dispatch({ type: "patch", index, patch }),
    [dispatch],
  );
  const onAdd = useCallback(() => dispatch({ type: "add" }), [dispatch]);
  const onDuplicate = useCallback(() => dispatch({ type: "duplicate" }), [dispatch]);
  const onDelete = useCallback(() => dispatch({ type: "remove" }), [dispatch]);
  const onSnapChange = useCallback((on: boolean) => dispatch({ type: "snap", on }), [dispatch]);
  const onLockChange = useCallback((on: boolean) => dispatch({ type: "lock", on }), [dispatch]);

  const save = async () => {
    try {
      await api().SaveConfig({ ...config, screens });
      dispatch({ type: "markSaved", message: "Saved — applied live" });
    } catch (e) {
      dispatch({ type: "fail", message: String(e) });
    }
  };

  return (
    <div className="flex h-full flex-col">
      <Toolbar
        percent={view.percent}
        minPercent={view.minPercent}
        maxPercent={view.maxPercent}
        onPercentChange={view.setPercent}
        onZoomBy={view.zoomBy}
        onFit={view.fit}
        snap={snap}
        onSnapChange={onSnapChange}
        lock={lock}
        onLockChange={onLockChange}
        onAdd={onAdd}
        onSave={() => void save()}
        dirty={dirty}
        savedMsg={savedMsg}
        error={error}
      />

      {issues.length > 0 && (
        <div className="border-b border-amber-500/30 bg-amber-500/10 px-8 py-2">
          <ul className="space-y-0.5">
            {issues.map((msg, i) => (
              <li key={i} className="text-xs text-amber-500">⚠ {msg}</li>
            ))}
          </ul>
        </div>
      )}

      <div className="flex min-h-0 flex-1">
        <Canvas
          view={view.view}
          viewportRef={view.viewportRef}
          screens={screens}
          selected={selected}
          lock={lock}
          snap={snap}
          onSelect={onSelect}
          onMove={onMove}
          zoomAtPoint={view.zoomAtPoint}
          panBy={view.panBy}
        />
        <ScreenInspector
          screen={selected >= 0 ? screens[selected] : null}
          index={selected}
          lock={lock}
          onPatch={onPatch}
          onAdd={onAdd}
          onDuplicate={onDuplicate}
          onDelete={onDelete}
        />
      </div>
    </div>
  );
}