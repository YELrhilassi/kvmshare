import { useCallback, useEffect, useState } from "react";
import { api, type LayoutConfig, type Screen } from "@/lib/bridge";
import Toolbar from "@/features/layout/Toolbar";
import Canvas from "@/features/layout/canvas/Canvas";
import ScreenInspector from "@/features/layout/inspector/ScreenInspector";
import { useLayoutDocument } from "@/features/layout/useLayoutDocument";
import { useCanvasView } from "@/features/layout/useCanvasView";
import { PageSkeleton } from "@/components/PageSkeleton";

export default function LayoutPage() {
  const [config, setConfig] = useState<LayoutConfig | null>(null);
  const [error, setError] = useState("");

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

  return <Editor config={config} />;
}

// The editor: one reducer for the document, one hook for the view, and
// three presentational pieces (toolbar / canvas / inspector). The full
// config is passed in so a save round-trips every field (port, network
// policy, …) — the layout page only ever edits screens.
function Editor({ config }: { config: LayoutConfig }) {
  const { state, dispatch } = useLayoutDocument(config.screens);
  const view = useCanvasView(state.screens);
  const { screens, selected, lock, snap, dirty, savedMsg, error } = state;

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