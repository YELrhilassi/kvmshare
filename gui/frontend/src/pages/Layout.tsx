import { useEffect, useState } from "react";
import { api, type LayoutConfig } from "@/lib/bridge";
import LayoutEditor from "@/components/LayoutEditor";

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
    return (
      <div className="flex h-full items-center justify-center">
        <p className="text-sm text-muted-foreground">Loading layout…</p>
      </div>
    );
  }

  return (
    <div className="h-full">
      <LayoutEditor initial={config} />
    </div>
  );
}