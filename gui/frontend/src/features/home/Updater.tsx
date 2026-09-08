import { useEffect, useState } from "react";
import { api } from "@/lib/bridge";

type UpdateState = "idle" | "checking" | "uptodate" | "available" | "applying" | "error";

// Version + self-update line. The real error is shown (not a generic
// "failed") with a retry, so a rate-limited or offline machine is
// understandable.
export default function Updater() {
  const [version, setVersion] = useState("");
  const [state, setState] = useState<UpdateState>("idle");
  const [newVersion, setNewVersion] = useState("");
  const [error, setError] = useState("");

  useEffect(() => {
    void api()
      .GetVersion()
      .then(setVersion)
      .catch(() => {});
  }, []);

  // Dev builds are not released: there is nothing to update from and no
  // honest "check" to run — show that plainly instead of a check button
  // that fails.
  const isDev = version === "" || version === "v0.0.0-dev" || version.endsWith("-dev");

  const check = async () => {
    setState("checking");
    setError("");
    try {
      const info = await api().CheckForUpdate();
      if (info.error) {
        setState("error");
        setError(info.error);
      } else if (info.available) {
        setNewVersion(info.version);
        setState("available");
      } else {
        setState("uptodate");
      }
    } catch (e) {
      setState("error");
      setError(String(e));
    }
  };

  const apply = async () => {
    setState("applying");
    const res = await api().ApplyUpdate();
    if (res.error) {
      setState("error");
      setError(res.error);
    }
    // Success: the backend restarts the app shortly; leave the line as is.
  };

  return (
    <div className="flex flex-wrap items-center gap-x-3 gap-y-2 border-t border-border/70 pt-5 text-xs text-muted-foreground/70">
      <span className="font-mono">{version || "kvmshare"}</span>
      <span className="h-3 w-px bg-border" />
      {isDev && <span>development build — updates disabled</span>}
      {!isDev && state === "idle" && (
        <button onClick={() => void check()} className="transition-colors hover:text-foreground">
          Check for updates
        </button>
      )}
      {state === "checking" && <span>Checking…</span>}
      {state === "uptodate" && <span>Up to date</span>}
      {state === "available" && (
        <button
          onClick={() => void apply()}
          className="font-medium text-foreground transition-colors hover:opacity-70"
        >
          Update to {newVersion} — install &amp; restart
        </button>
      )}
      {state === "applying" && <span>Installing — restarting…</span>}
      {state === "error" && (
        <span className="flex items-center gap-3">
          <span className="text-destructive" title={error}>
            {error || "Update check failed"}
          </span>
          <button onClick={() => void check()} className="transition-colors hover:text-foreground">
            Retry
          </button>
        </span>
      )}
    </div>
  );
}