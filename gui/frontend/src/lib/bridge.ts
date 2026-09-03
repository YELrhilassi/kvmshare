// Typed bridge over the Wails `window.go` runtime injection.
//
// Wails generates per-call JS bindings for convenience; we call the
// underlying `window.go.main.App.*` methods directly with our own types,
// which keeps the frontend dependency-free and the types exact.

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
}

declare global {
  interface Window {
    go: { main: { App: GoApp } };
  }
}

export const api = (): GoApp => window.go.main.App;