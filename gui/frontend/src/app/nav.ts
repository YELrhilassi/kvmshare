import type { Mode } from "@/lib/bridge";

// A machine runs one role at a time, and the UI follows: in server mode
// only server pages exist (plus Home and the Layout it owns); in client
// mode only the client page. Nothing mixes — the other role's settings
// live behind the role switch on Home.
export type Page = "home" | "server" | "client" | "layout" | "logs" | "settings";

export const NAV: { id: Page; label: string }[] = [
  { id: "home", label: "Home" },
  { id: "server", label: "Server" },
  { id: "client", label: "Client" },
  { id: "layout", label: "Layout" },
  { id: "logs", label: "Logs" },
  { id: "settings", label: "Settings" },
];

export function pagesFor(mode: Mode): Page[] {
  // Settings is role-independent: it configures the machine, not a role.
  return mode === "server"
    ? ["home", "server", "layout", "logs", "settings"]
    : ["home", "client", "logs", "settings"];
}