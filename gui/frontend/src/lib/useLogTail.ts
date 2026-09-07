import { useCallback, useEffect, useRef, useState } from "react";
import { api } from "@/lib/bridge";

// Live-tail a log file: poll every 1.5s, stick to the bottom unless the
// user has scrolled up. Pass `undefined` while the log path is unknown.
export function useLogTail(path: string | undefined, maxLines = 300) {
  const [log, setLog] = useState("");
  const [stick, setStick] = useState(true);
  const viewportRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    setLog("");
    setStick(true);
    if (!path) return;
    let alive = true;
    const tick = async () => {
      try {
        const text = await api().TailLog(path, maxLines);
        if (!alive) return;
        setLog(text);
        if (stick) {
          requestAnimationFrame(() => {
            const el = viewportRef.current;
            if (el) el.scrollTop = el.scrollHeight;
          });
        }
      } catch {
        /* log not available yet */
      }
    };
    void tick();
    const id = setInterval(tick, 1500);
    return () => {
      alive = false;
      clearInterval(id);
    };
  }, [path, maxLines, stick]);

  const onScroll = useCallback(() => {
    const el = viewportRef.current;
    if (!el) return;
    const nearBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 60;
    if (nearBottom !== stick) setStick(nearBottom);
  }, [stick]);

  return { log, viewportRef, onScroll, stick, setStick };
}