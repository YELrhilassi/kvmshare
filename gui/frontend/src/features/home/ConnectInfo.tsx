import { useEffect, useState } from "react";
import { useApp } from "@/app/AppProvider";
import { api, copyText } from "@/lib/bridge";
import { DEFAULT_PORT } from "@/lib/constants";
import { lanAddresses } from "@/lib/net";
import { Section } from "@/components/Section";
import { shortID } from "@/lib/utils";

// The two facts another machine needs about this one: where to reach it
// (server) and what identity to trust (both roles). Nothing here
// repeats the status — the id is the one thing people actually have to
// find and type, so it gets a copy button.
export default function ConnectInfo() {
  const { mode, clientName } = useApp();
  const [ips, setIps] = useState<string[]>([]);
  const [port, setPort] = useState(DEFAULT_PORT);
  const [machineId, setMachineId] = useState("");
  const [copied, setCopied] = useState(false);

  const isServer = mode === "server";

  useEffect(() => {
    void api()
      .GetMachineId()
      .then(setMachineId)
      .catch(() => {});
    void api()
      .ListInterfaces()
      .then((ifaces) => setIps(lanAddresses(ifaces)))
      .catch(() => {});
    if (isServer) {
      void api()
        .LoadConfig()
        .then((c) => setPort(c.port))
        .catch(() => {});
    }
  }, [isServer]);

  const copy = async () => {
    if (!machineId) return;
    await copyText(machineId).catch(() => {});
    setCopied(true);
    window.setTimeout(() => setCopied(false), 1500);
  };

  const idBlurb = isServer
    ? "Other machines check this id when you ask them to connect."
    : "Add this id on the server to let this machine connect.";

  return (
    <Section title={isServer ? "How others connect" : "About this machine"}>
      {isServer ? (
        <div className="space-y-3">
          <div className="space-y-1">
            <div className="text-xs uppercase tracking-wide text-muted-foreground/60">Other machines connect to</div>
            {ips.map((ip) => (
              <div key={ip} className="font-mono text-lg">
                {ip}:{port}
              </div>
            ))}
            {ips.length === 0 && <div className="font-mono text-lg text-muted-foreground">no network address found</div>}
          </div>
          <MachineId id={machineId} short={shortID(machineId)} copied={copied} onCopy={copy} blurb={idBlurb} />
        </div>
      ) : (
        <div className="space-y-3">
          <div className="space-y-1">
            <div className="text-xs uppercase tracking-wide text-muted-foreground/60">Shows up on the server as</div>
            <div className="text-lg">
              <span className="font-mono">{clientName || "—"}</span>
            </div>
          </div>
          <MachineId id={machineId} short={shortID(machineId)} copied={copied} onCopy={copy} blurb={idBlurb} />
        </div>
      )}
    </Section>
  );
}

// MachineId shows the short id with a copy button (copies the full id)
// and a one-line note on where the id is used.
function MachineId({
  id,
  short,
  copied,
  onCopy,
  blurb,
}: {
  id: string;
  short: string;
  copied: boolean;
  onCopy: () => void;
  blurb: string;
}) {
  if (!id) return null;
  return (
    <div className="space-y-1">
      <div className="text-xs uppercase tracking-wide text-muted-foreground/60">Machine id</div>
      <div className="flex items-center gap-2">
        <span className="font-mono text-lg">{short}</span>
        <button
          onClick={onCopy}
          title="Copy full id"
          className="rounded border border-border/70 px-2 py-0.5 font-mono text-[11px] text-muted-foreground transition-colors hover:bg-muted/50 hover:text-foreground"
        >
          {copied ? "copied" : "copy"}
        </button>
      </div>
      <p className="text-xs text-muted-foreground/70">{blurb}</p>
    </div>
  );
}