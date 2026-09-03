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

export interface LayoutConfig {
  port: number;
  screens: Screen[];
}

export interface Settings {
  mode: Mode;
  clientAddr: string;
  clientName: string;
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
  GetVersion(): Promise<string>;
  CheckForUpdate(): Promise<UpdateInfo>;
  ApplyUpdate(): Promise<UpdateResult>;
}

interface WailsCall {
  ByName(method: string, ...args: unknown[]): Promise<unknown>;
}

declare global {
  interface Window {
    /** Wails v3 runtime, injected into the page at startup. */
    wails?: { Call: WailsCall };
  }
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
  GetVersion: () => call<string>("GetVersion"),
  CheckForUpdate: () => call<UpdateInfo>("CheckForUpdate"),
  ApplyUpdate: () => call<UpdateResult>("ApplyUpdate"),
});
