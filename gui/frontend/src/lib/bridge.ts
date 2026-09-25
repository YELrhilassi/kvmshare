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
  /** [shortcuts] config section — schema owned by the Rust server. */
  shortcuts?: ShortcutSection;
  /** [input] config section — schema owned by the Rust server. */
  input?: InputSection;
}

export interface ShortcutSection {
  enabled: boolean;
  bindings: Binding[];
}

/** One shortcut chord: modifier set + canonical HID key + action. */
export interface Binding {
  mods: { ctrl: boolean; alt: boolean; shift: boolean; meta: boolean };
  key: number;
  /** switch (needs screen) | cycle | lock | home */
  action: string;
  screen?: string;
}

export interface InputSection {
  pointerSpeed: number;
  wheelSpeed: number;
  swapScroll: boolean;
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
  /** The peer's advertised role is actually running there (a GUI up but idle shows inactive). */
  active: boolean;
}

/** The discovery duty-cycle state: what the engine is doing right now. */
export interface DiscoveryStatus {
  /** "idle" (nothing running), "seeking" (full-rate hunt), "connected" (partner present, low rate). */
  state: "idle" | "seeking" | "connected";
  /** Consecutive sessions that ended without hearing anyone (0 = healthy). */
  failedSessions: number;
  /** Seconds left in the current session (0 when idle). */
  secondsLeft: number;
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

// How this machine's role processes get their input privileges. On
// Windows the roles must run elevated to reach elevated windows (Task
// Manager, installers); the GUI reports whether that is working.
export interface RoleElevation {
  elevated: boolean;
  canElevate: boolean;
  detail?: string;
}

export interface ClientState {
  status: "connected" | "connecting" | "disconnected" | "refused";
  server: string;
  /** The server's machine id from its Welcome (set on "connected"). */
  serverId?: string;
  /** The server's refusal explanation when status is "refused". */
  reason?: string;
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
  /** Machine ids this machine refuses (revoked wins over trusted). */
  revoked: string[];
  /** True while this machine's physical input devices are grabbed away
   *  (driven from another machine). Written by the role process on
   *  every crossing; the resting state (control at home) is a missing
   *  file, so it reads false. */
  controlAway: boolean;
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
//
// The subscription is genuinely removable: Wails v3's `Events.Off(name)`
// removes the listeners registered for a name, and this app has exactly
// one subscriber per name (AppProvider owns `kvmshare:state`), so
// Off(name) is a full unsubscribe, not a clobber of other consumers.
// The async race is covered too: a disposer called before the runtime
// finished loading also cancels the pending `On` (the old version only
// set a flag, so mount/unmount cycles accumulated listeners forever
// once the runtime had loaded).
export function onState(cb: (s: LiveSnapshot) => void): () => void {
  let disposed = false;
  let handler: ((e: WailsEvent<LiveSnapshot>) => void) | undefined;
  void runtime().then(() => {
    if (disposed) return;
    const ev = window.wails?.Events;
    if (!ev?.On) return;
    handler = (e: WailsEvent<LiveSnapshot>) => cb(e.data);
    ev.On(stateEventName, handler);
  });
  return () => {
    disposed = true;
    if (handler) {
      window.wails?.Events?.Off(stateEventName);
      handler = undefined;
    }
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
  // Direct probe of one address (host, or host:port) — the manual path
  // of last resort; errors name the address and why it did not answer.
  ProbeHost(addr: string): Promise<Peer>;
  // The discovery duty-cycle state (idle/seeking/connected, failed
  // session count, seconds left) for the "On this network" header.
  DiscoveryStatus(): Promise<DiscoveryStatus>;
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
  // Role elevation (meaningful on Windows): whether role processes run
  // elevated, and what stands in the way when they do not.
  RoleElevation(): Promise<RoleElevation>;
  // Shortcut-recording key capture (Windows): a system-wide keyboard
  // hook active only during a recording session, so OS-bound chords
  // (Win+Tab) can be seen and swallowed before the shell acts.
  StartKeyCapture(token: string): Promise<string>;
  StopKeyCapture(token: string): Promise<void>;
  RenewKeyCapture(token: string): Promise<void>;
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
  ProbeHost: (addr) => call<Peer>("ProbeHost", addr),
  DiscoveryStatus: () => call<DiscoveryStatus>("DiscoveryStatus"),
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
  RoleElevation: () => call<RoleElevation>("RoleElevation"),
  StartKeyCapture: (token) => call<string>("StartKeyCapture", token),
  StopKeyCapture: (token) => call<void>("StopKeyCapture", token),
  RenewKeyCapture: (token) => call<void>("RenewKeyCapture", token),
});
