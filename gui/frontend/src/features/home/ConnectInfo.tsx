import { useEffect, useState } from "react";
import { useApp } from "@/app/AppProvider";
import { api, type Settings } from "@/lib/bridge";
import { DEFAULT_PORT } from "@/lib/constants";
import { lanAddresses } from "@/lib/net";
import { Section } from "@/components/Section";

// What this machine is reachable at / reaching out to — the numbers a
// person actually needs when setting the other end up.
export default function ConnectInfo() {
  const { mode } = useApp();
  const [ips, setIps] = useState<string[]>([]);
  const [port, setPort] = useState(DEFAULT_PORT);
  const [settings, setSettings] = useState<Settings | null>(null);

  useEffect(() => {
    void api()
      .ListInterfaces()
      .then((ifaces) => setIps(lanAddresses(ifaces)))
      .catch(() => {});
    if (mode === "server") {
      void api()
        .LoadConfig()
        .then((c) => setPort(c.port))
        .catch(() => {});
    } else {
      void api()
        .GetSettings()
        .then(setSettings)
        .catch(() => {});
    }
  }, [mode]);

  return (
    <Section title={mode === "server" ? "Other machines connect to" : "This machine connects to"}>
      {mode === "server" ? (
        <div className="space-y-1">
          {ips.map((ip) => (
            <div key={ip} className="font-mono text-lg">
              {ip}:{port}
            </div>
          ))}
          {ips.length === 0 && (
            <div className="font-mono text-lg text-muted-foreground">no network address found</div>
          )}
        </div>
      ) : (
        settings && (
          <div className="text-lg">
            <span className="font-mono">{settings.clientAddr || "—"}</span>
            <span className="mx-2 text-muted-foreground">as</span>
            <span className="font-mono">{settings.clientName || "—"}</span>
          </div>
        )
      )}
    </Section>
  );
}