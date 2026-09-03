// Navigation model for the app shell.
export type Page = "home" | "server" | "client" | "layout";

export const NAV: { id: Page; label: string }[] = [
  { id: "home", label: "Home" },
  { id: "server", label: "Server" },
  { id: "client", label: "Client" },
  { id: "layout", label: "Layout" },
];