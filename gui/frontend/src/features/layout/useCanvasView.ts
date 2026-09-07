import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import type { Screen } from "@/lib/bridge";
import {
  boundsOf,
  centeredView,
  fitScale,
  MAX_ZOOM,
  MIN_ZOOM,
  toModel,
  viewAtPercent,
  zoomAt,
  type View,
} from "@/features/layout/geometry";

const DEFAULT_VIEW: View = { scale: 0.5, pan: { x: 40, y: 40 } };

// Owns the canvas view (scale + pan). All the transient interaction
// reads go through `viewRef`, so gesture handlers never re-bind; the
// only ref is the view itself, kept in sync by `update`.
export function useCanvasView(screens: Screen[]) {
  const viewportRef = useRef<HTMLDivElement>(null);
  const [view, setView] = useState<View>(DEFAULT_VIEW);
  const viewRef = useRef(view);
  const fittedRef = useRef(false); // initial auto-fit done with a real size?
  const userAdjustedRef = useRef(false); // user took over the view?

  const update = useCallback((next: View) => {
    viewRef.current = next;
    setView(next);
  }, []);

  const markAdjusted = useCallback(() => {
    userAdjustedRef.current = true;
    fittedRef.current = true;
  }, []);

  const viewportSize = useCallback(() => {
    const el = viewportRef.current;
    return el ? { w: el.clientWidth, h: el.clientHeight } : { w: 800, h: 600 };
  }, []);

  // Fit: frame the whole desktop. Screens are real pixels in the
  // document but model units on the canvas, so fit works on the
  // model-space bounds — a 1080p desktop fits comfortably at 1:1 model
  // scale instead of shrinking to a sliver. The resulting zoom shows as
  // its true percentage (e.g. 45%), not as "100%".
  const fit = useCallback(() => {
    const v = viewportSize();
    const b = boundsOf(screens.map(toModel));
    update(centeredView(fitScale(v, b), v, b));
  }, [screens, viewportSize, update]);

  // Zoom by a factor, keeping the viewport center fixed (toolbar +/-).
  const zoomBy = useCallback(
    (factor: number) => {
      markAdjusted();
      const v = viewportSize();
      update(zoomAt(viewRef.current, factor, { x: v.w / 2, y: v.h / 2 }));
    },
    [markAdjusted, viewportSize, update],
  );

  // Zoom by a factor, keeping the world point under a client coordinate
  // inside the viewport fixed (mouse wheel).
  const zoomAtPoint = useCallback(
    (factor: number, clientX: number, clientY: number) => {
      markAdjusted();
      const el = viewportRef.current;
      if (!el) return;
      const rect = el.getBoundingClientRect();
      update(
        zoomAt(viewRef.current, factor, {
          x: clientX - rect.left,
          y: clientY - rect.top,
        }),
      );
    },
    [markAdjusted, viewportSize, update],
  );

  // Set the zoom percentage (100 = model 1:1).
  const setPercent = useCallback(
    (pct: number) => {
      markAdjusted();
      update(viewAtPercent(viewRef.current, viewportSize(), pct));
    },
    [markAdjusted, viewportSize, update],
  );

  // Pan by a screen-pixel delta (pan gesture).
  const panBy = useCallback(
    (dx: number, dy: number) => {
      markAdjusted();
      const cur = viewRef.current;
      update({ ...cur, pan: { x: cur.pan.x + dx, y: cur.pan.y + dy } });
    },
    [markAdjusted, update],
  );

  // Fit once the canvas is really measurable. WebKit can report a wrong
  // (tiny) viewport during startup, so retry until a sane size shows up
  // and never fight the user afterwards.
  const fitWhenReady = useCallback(() => {
    if (fittedRef.current) return;
    const el = viewportRef.current;
    if (!el || screens.length === 0) return;
    if (el.clientWidth < 50 || el.clientHeight < 50) return;
    fittedRef.current = true;
    fit();
  }, [fit, screens]);

  useLayoutEffect(() => {
    fitWhenReady();
  }, [fitWhenReady]);

  // Refit on every real size change until the user takes over the view,
  // so the first render is always the fitted one no matter when we
  // measured.
  useEffect(() => {
    const el = viewportRef.current;
    if (!el) return;
    const t1 = window.setTimeout(fitWhenReady, 150);
    const t2 = window.setTimeout(fitWhenReady, 600);
    let lastW = -1;
    let lastH = -1;
    const ro = new ResizeObserver(() => {
      const w = el.clientWidth;
      const h = el.clientHeight;
      if (w === lastW && h === lastH) {
        fitWhenReady();
        return;
      }
      lastW = w;
      lastH = h;
      if (userAdjustedRef.current) return;
      fittedRef.current = true;
      fit();
    });
    ro.observe(el);
    return () => {
      window.clearTimeout(t1);
      window.clearTimeout(t2);
      ro.disconnect();
    };
  }, [fitWhenReady, fit]);

  // Zoom is a fixed, balanced range around model 1:1 — never dependent
  // on how big or spread out the layout happens to be.
  const percent = view.scale * 100;
  const minPercent = MIN_ZOOM * 100;
  const maxPercent = MAX_ZOOM * 100;

  return {
    view,
    viewRef,
    viewportRef,
    fit,
    zoomBy,
    zoomAtPoint,
    setPercent,
    panBy,
    percent,
    minPercent,
    maxPercent,
  };
}