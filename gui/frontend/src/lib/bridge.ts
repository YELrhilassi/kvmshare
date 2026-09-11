// Typed bridge over the Wails runtime.
//
// Wails v3 injects the runtime into the page as `window.wails` when the
// app starts. Bound Go methods are addressed by their fully qualified
// name — `<package>.<Type>.<Method>` — where `<package>` is the name
// under which the type is registered. The GUI's service is the `App`
// type in `package main`, which Wails registers as `main.App` (types in
// the main package report `main` as their package path). Keeping that
// prefix in one constant means the wire names live in exactly one place;
// the interfaces below mirror the Go structs one-to-one.

const APP_SERVICE = "main.App"; // the bound service type in main.go

export type Mode = "server" | "client";

export interface Screen {
  name: string;
  width: number;
  height: number;
  x: number;
  y: number;
}

export interface Network {
  allowlist: boolean;
  localOnly: boolean;
  trustedIds: string[];
  // Ids that may never connect. Independent of trustedIds — the same id
  // may be in both, and a revoked id always loses.
  revokedIds: string[];
}

export interface LayoutConfig {
  port: number;
  screens: Screen[];
  network: Network;
}

export interface Settings {
  mode: Mode;
  clientAddr: string;
  clientName: string;
  logLevel: string;
  logEnabled: boolean;
  trustedServers: string[];
  // Server-managed (set by revoke/trust and by ending a session): the
  // settings form never edits these, and the backend preserves them
  // across a write.
  revokedServers: string[];
  acceptPairing: boolean;
  autoConnect: boolean;
  autoConnectPaused: boolean;
  // General settings (Settings page): the OS autostart entry is written
  // by its own bound methods; this flag mirrors it for the toggle. The
  // start role is what the machine runs at GUI launch ("" = nothing).
  launchAtStartup: boolean;
  startRole: "" | "server" | "client";
  // Open minimized to the tray on launch (only honored when a tray host
  // exists — the backend enforces that, the flag is just the wish).
  startHidden?: boolean;
}

export interface ConnectedClient {
  name: string;
  id: string;
  addr: string;
  sinceMs: number;
}

export interface Peer {
  id: string;
  name: string;
  role: "server" | "client";
  addr: string;
  port: number;
  source: string;
}

export interface LogSettings {
  role: "server" | "client";
  level: string;
  enabled: boolean;
}

export interface InterfaceInfo {
  name: string;
  addrs: string[];
}

export interface Paths {
  configPath: string;
  serverLog: string;
  clientLog: string;
  serverBin: string;
  clientBin: string;
}

export interface UpdateInfo {
  current: string;
  available: boolean;
  version: string;
  error?: string;
}

export interface UpdateResult {
  restarting: boolean;
  error?: string;
}

export interface ClientState {
  status: "connected" | "connecting" | "disconnected";
  server: string;
  /** When the current connecting run began (unix ms, 0 when not connecting). */
  connectingSinceMs: number;
}

export interface LiveSnapshot {
  mode: Mode;
  clientName: string;
  running: { server: boolean; client: boolean };
  clientState: ClientState;
  peers: Peer[];
  clients: ConnectedClient[];
  /** Machine ids this machine trusts (config ids on a server, trusted-server ids on a client). */
  trusted: string[];
}

/** The Wails event object delivered to `Events.On` callbacks. */
interface WailsEvent<T> {
  name: string;
  data: T;
}

const stateEventName = "kvmshare:state";

// Subscribe to the backend's live-state stream. The backend emits a
// `kvmshare:state` snapshot only when something actually changed, so the
// page renders real transitions without polling the bridge. Returns an
// unsubscribe function.
export function onState(cb: (s: LiveSnapshot) => void): () => void {
  let disposed = false;
  void runtime().then(() => {
    if (disposed) return;
    const ev = window.wails?.Events;
    if (!ev?.On) return;
    ev.On(stateEventName, (e: WailsEvent<LiveSnapshot>) => cb(e.data));
  });
  return () => {
    disposed = true;
  };
}

interface GoApp {
  GetSettings(): Promise<Settings>;
  SetSettings(s: Settings): Promise<void>;
  GetPaths(): Promise<Paths>;
  LoadConfig(): Promise<LayoutConfig>;
  SaveConfig(c: LayoutConfig): Promise<void>;
  ServerStart(): Promise<boolean>;
  ServerStop(): Promise<void>;
  ServerRunning(): Promise<boolean>;
  ClientStart(): Promise<boolean>;
  ClientStop(): Promise<void>;
  ClientRunning(): Promise<boolean>;
  StartActive(): Promise<boolean>;
  StopActive(): Promise<void>;
  ListInterfaces(): Promise<InterfaceInfo[]>;
  TailLog(path: string, lines: number): Promise<string>;
  GetLogSettings(): Promise<LogSettings>;
  SetLogSettings(s: LogSettings): Promise<void>;
  ClearLog(role: string): Promise<void>;
  GetVersion(): Promise<string>;
  CheckForUpdate(): Promise<UpdateInfo>;
  ApplyUpdate(): Promise<UpdateResult>;
  GetMachineId(): Promise<string>;
  DiscoverPeers(): Promise<Peer[]>;
  RefreshDiscovery(): Promise<Peer[]>;
  ListClients(): Promise<ConnectedClient[]>;
  ClientCommand(name: string, action: string): Promise<void>;
  // Each setter is idempotent: pass the desired membership, not a toggle.
  TrustClient(id: string, trusted: boolean): Promise<void>;
  RevokeClient(id: string, revoked: boolean): Promise<void>;
  TrustServer(id: string, trusted: boolean): Promise<void>;
  RevokeServer(id: string, revoked: boolean): Promise<void>;
  ConnectToServer(addr: string): Promise<void>;
  SendConnectRequest(peerId: string): Promise<void>;
  ClientStatus(): Promise<ClientState>;
  // Launch-at-startup: writes/removes the OS entry (XDG autostart file
  // or the per-user Run key) and mirrors the flag into the settings.
  EnableLaunchAtStartup(): Promise<void>;
  DisableLaunchAtStartup(): Promise<void>;
  LaunchAtStartupEnabled(): Promise<boolean>;
}

interface WailsCall {
  ByName(method: string, ...args: unknown[]): Promise<unknown>;
}

interface WailsEvents {
  On<T>(name: string, cb: (e: WailsEvent<T>) => void): void;
  Off(name: string): void;
}

interface WailsClipboard {
  SetText(text: string): Promise<void>;
}

declare global {
  interface Window {
    /** Wails v3 runtime, injected into the page at startup. */
    wails?: { Call: WailsCall; Events: WailsEvents; Clipboard: WailsClipboard };
  }
}

// Copy text to the system clipboard — via the Wails runtime when
// present, with a DOM fallback for browser development.
export async function copyText(text: string): Promise<void> {
  const clip = window.wails?.Clipboard;
  if (clip?.SetText) {
    await clip.SetText(text);
    return;
  }
  await navigator.clipboard.writeText(text);
}

// The full runtime is served by the app at /wails/runtime.js; pages are
// expected to load it themselves (the generated bindings do). Load it
// explicitly on first use, then wait briefly for `window.wails`.
let runtimePromise: Promise<WailsCall> | null = null;
function runtime(): Promise<WailsCall> {
  if (!runtimePromise) {
    runtimePromise = (async () => {
      if (!window.wails?.Call?.ByName) {
        // @vite-ignore: resolved by the app's asset server at runtime.
        await import(/* @vite-ignore */ "/wails/runtime.js" as string).catch(() => {});
      }
      const deadline = Date.now() + 5000;
      while (!window.wails?.Call?.ByName) {
        if (Date.now() > deadline) {
          throw new Error("kvmshare: wails runtime did not load");
        }
        await new Promise((r) => window.setTimeout(r, 25));
      }
      return window.wails.Call;
    })();
  }
  return runtimePromise;
}

function call<T>(method: string, ...args: unknown[]): Promise<T> {
  return runtime().then((r) => r.ByName(`${APP_SERVICE}.${method}`, ...args)) as Promise<T>;
}

export const api = (): GoApp => ({
  GetSettings: () => call<Settings>("GetSettings"),
  SetSettings: (s) => call<void>("SetSettings", s),
  GetPaths: () => call<Paths>("GetPaths"),
  LoadConfig: () => call<LayoutConfig>("LoadConfig"),
  SaveConfig: (c) => call<void>("SaveConfig", c),
  ServerStart: () => call<boolean>("ServerStart"),
  ServerStop: () => call<void>("ServerStop"),
  ServerRunning: () => call<boolean>("ServerRunning"),
  ClientStart: () => call<boolean>("ClientStart"),
  ClientStop: () => call<void>("ClientStop"),
  ClientRunning: () => call<boolean>("ClientRunning"),
  StartActive: () => call<boolean>("StartActive"),
  StopActive: () => call<void>("StopActive"),
  ListInterfaces: () => call<InterfaceInfo[]>("ListInterfaces"),
  TailLog: (path, lines) => call<string>("TailLog", path, lines),
  GetLogSettings: () => call<LogSettings>("GetLogSettings"),
  SetLogSettings: (s) => call<void>("SetLogSettings", s),
  ClearLog: (role) => call<void>("ClearLog", role),
  GetVersion: () => call<string>("GetVersion"),
  CheckForUpdate: () => call<UpdateInfo>("CheckForUpdate"),
  ApplyUpdate: () => call<UpdateResult>("ApplyUpdate"),
  GetMachineId: () => call<string>("GetMachineId"),
  DiscoverPeers: () => call<Peer[]>("DiscoverPeers"),
  RefreshDiscovery: () => call<Peer[]>("RefreshDiscovery"),
  ListClients: () => call<ConnectedClient[]>("ListClients"),
  ClientCommand: (name, action) => call<void>("ClientCommand", name, action),
  TrustClient: (id, trusted) => call<void>("TrustClient", id, trusted),
  RevokeClient: (id, revoked) => call<void>("RevokeClient", id, revoked),
  TrustServer: (id, trusted) => call<void>("TrustServer", id, trusted),
  RevokeServer: (id, revoked) => call<void>("RevokeServer", id, revoked),
  ConnectToServer: (addr) => call<void>("ConnectToServer", addr),
  SendConnectRequest: (peerId) => call<void>("SendConnectRequest", peerId),
  ClientStatus: () => call<ClientState>("ClientStatus"),
  EnableLaunchAtStartup: () => call<void>("EnableLaunchAtStartup"),
  DisableLaunchAtStartup: () => call<void>("DisableLaunchAtStartup"),
  LaunchAtStartupEnabled: () => call<boolean>("LaunchAtStartupEnabled"),
});
