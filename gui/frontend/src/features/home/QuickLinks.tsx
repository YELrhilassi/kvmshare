import { useEffect, useState } from "react";
import { useApp } from "@/app/AppProvider";
import { api } from "@/lib/bridge";
import { pagesFor, type Page } from "@/app/nav";
import { Section } from "@/components/Section";

interface Props {
  onNavigate: (p: Page) => void;
}

// Shortcuts into the pages that belong to the current role.
export default function QuickLinks({ onNavigate }: Props) {
  const { mode } = useApp();
  const [screens, setScreens] = useState(0);

  useEffect(() => {
    if (mode === "server") {
      void api()
        .LoadConfig()
        .then((c) => setScreens(c.screens.length))
        .catch(() => {});
    }
  }, [mode]);

  const links = pagesFor(mode).filter((p) => p !== "home");

  return (
    <Section title="Quick access">
      <div className="flex flex-wrap gap-x-10 gap-y-3">
        {links.map((p) => (
          <button
            key={p}
            onClick={() => onNavigate(p)}
            className="text-sm text-muted-foreground transition-colors hover:text-foreground"
          >
            {p === "layout" ? `Layout · ${screens} screens` : p === "server" ? "Server settings" : p === "client" ? "Client settings" : "Logs"}
          </button>
        ))}
      </div>
    </Section>
  );
}