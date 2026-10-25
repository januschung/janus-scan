// Typed wrappers over the scanner commands, with two transports: Tauri
// commands (src-tauri/src/lib.rs) for the desktop app, and HTTP plus
// Server-Sent Events (crates/server) for the web build. `VITE_JANUS_WEB=1`
// at build time picks the web one; the other is dropped from the bundle.
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export const isWeb = import.meta.env.VITE_JANUS_WEB === "1";

export type Connection = "usb" | "network";
export type Source = "feeder" | "flatbed";
export type ColorMode = "color" | "gray" | "bw";
export type PaperKind = "letter" | "legal" | "a4" | "a5" | "business-card" | "receipt" | "auto";
export type Format = "pdf" | "jpeg" | "png" | "tiff";

export interface DeviceInfo {
  id: string;
  name: string;
  serial: string | null;
  connection: Connection;
  backend: string;
}

export interface Capabilities {
  sources: Source[];
  duplex: boolean;
  resolutions: number[];
  color_modes: ColorMode[];
  max_size_mm: [number, number];
}

export interface PageView {
  id: number;
  width: number;
  height: number;
  dpi: number;
  /** A data URL on the desktop, a server URL on the web; either works as an `<img src>`. */
  thumb: string;
}

export interface ScanOptions {
  request: { source: Source; duplex: boolean; dpi: number; color: ColorMode; paper: { kind: PaperKind } };
  skip_blank: boolean;
  blank_sensitivity: number;
  auto_crop: boolean;
  deskew: boolean;
}

export interface ExportOptions {
  format: Format;
  jpeg_quality: number;
  multi_page_tiff: boolean;
  pdf_title: string | null;
}

/** A NAS folder from the server's config file (web only). */
export interface Destination {
  name: string;
  /** The folder's own file name preset, if it has one. */
  name_template: string | null;
}

export type ScanUpdate =
  | { kind: "page"; page: PageView }
  | { kind: "skipped"; index: number }
  | { kind: "feeder-empty" }
  | { kind: "done" }
  | { kind: "error"; error: { kind: string }; message: string };

type Unlisten = () => void;

interface Api {
  listDevices(): Promise<DeviceInfo[]>;
  openDevice(id: string): Promise<Capabilities>;
  startScan(options: ScanOptions): Promise<void>;
  cancelScan(): Promise<void>;
  listPages(): Promise<PageView[]>;
  /** An image URL for the page, longest side about `maxSide`. */
  preview(id: number, maxSide: number): Promise<string>;
  rotate(id: number, clockwise: boolean): Promise<PageView>;
  /** Removes pages from the open document only; saved files are untouched. */
  remove(ids: number[]): Promise<void>;
  reorder(order: number[]): Promise<void>;
  clear(): Promise<void>;
  /**
   * Saves the document and returns the file names. `target` is a folder on
   * the desktop; on the web a destination's name, or "" to download.
   */
  save(target: string, nameTemplate: string, opts: ExportOptions): Promise<string[]>;
  destinations(): Promise<Destination[]>;
  estimate(opts: ExportOptions): Promise<number>;
  defaultDir(): Promise<string | null>;
  onScan(cb: (u: ScanUpdate) => void): Promise<Unlisten>;
  /** The whole page list, whenever someone changes the shared document (web only). */
  onPages(cb: (pages: PageView[]) => void): Unlisten;
  /** Whether a scan pass is running, from any browser (web only). */
  onScanning(cb: (scanning: boolean) => void): Unlisten;
  /** The server removed unsaved pages after this many idle minutes (web only). */
  onExpired(cb: (minutes: number) => void): Unlisten;
}

const desktop: Api = {
  listDevices: () => invoke<DeviceInfo[]>("list_devices"),
  openDevice: (id) => invoke<Capabilities>("open_device", { id }),
  startScan: (options) => invoke<void>("start_scan", { options }),
  cancelScan: () => invoke<void>("cancel_scan"),
  listPages: () => invoke<PageView[]>("list_pages"),
  preview: (id, maxSide) => invoke<string>("page_preview", { id, maxSide }),
  rotate: (id, clockwise) => invoke<PageView>("rotate_page", { id, clockwise }),
  remove: (ids) => invoke<void>("remove_pages", { ids }),
  reorder: (order) => invoke<void>("reorder_pages", { order }),
  clear: () => invoke<void>("clear_pages"),
  save: (dir, nameTemplate, opts) =>
    invoke<string[]>("save_document", { options: { dir, name_template: nameTemplate, ...opts } }),
  estimate: (opts) => invoke<number>("estimate_size", { options: opts }),
  defaultDir: () => invoke<string | null>("default_save_dir"),
  destinations: async () => [],
  onScan: (cb): Promise<UnlistenFn> => listen<ScanUpdate>("scan", (e) => cb(e.payload)),
  onPages: () => () => {},
  onScanning: () => () => {},
  onExpired: () => () => {},
};

// ---- Web ------------------------------------------------------------------

async function request(method: string, path: string, body?: unknown): Promise<Response> {
  const res = await fetch(path, {
    method,
    headers: body === undefined ? undefined : { "content-type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  if (!res.ok) {
    const err = await res.json().catch(() => null);
    throw err?.error ?? `The server answered ${res.status} ${res.statusText}.`;
  }
  return res;
}

async function http<T>(method: string, path: string, body?: unknown): Promise<T> {
  const res = await request(method, path, body);
  return res.status === 204 || res.status === 202 ? (undefined as T) : res.json();
}

/** The file name from `Content-Disposition`, preferring the UTF-8 form. */
function attachmentName(header: string | null): string {
  const utf8 = header?.match(/filename\*=UTF-8''([^;]+)/i);
  if (utf8) return decodeURIComponent(utf8[1]);
  return header?.match(/filename="([^"]+)"/i)?.[1] ?? "scan";
}

/** Hands a file to the browser's own download handling (Downloads on Android). */
function download(blob: Blob, name: string) {
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = name;
  document.body.append(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(url), 60_000);
}

/** One event stream per page, shared by every listener; the browser reconnects it on its own. */
let source: EventSource | null = null;
function on<T>(event: string, cb: (data: T) => void): Unlisten {
  source ??= new EventSource("/api/events");
  const handler = (e: MessageEvent) => cb(JSON.parse(e.data));
  source.addEventListener(event, handler);
  return () => source?.removeEventListener(event, handler);
}

let scanner: { device: DeviceInfo; capabilities: Capabilities } | null = null;

const web: Api = {
  // The server drives exactly one scanner.
  async listDevices() {
    try {
      scanner = await http("GET", "/api/scanner");
      return [scanner!.device];
    } catch {
      scanner = null;
      return [];
    }
  },
  async openDevice(id) {
    if (scanner?.device.id !== id) await web.listDevices();
    if (!scanner) throw "Scanner not found.";
    return scanner.capabilities;
  },
  startScan: (options) => http("POST", "/api/scan", options),
  cancelScan: () => http("POST", "/api/scan/cancel"),
  listPages: () => http("GET", "/api/pages"),
  preview: async (id, maxSide) => `/api/pages/${id}/preview?max_side=${maxSide}&t=${Date.now()}`,
  rotate: (id, clockwise) => http("PATCH", `/api/pages/${id}`, { rotate: clockwise ? "cw" : "ccw" }),
  async remove(ids) {
    for (const id of ids) await http("DELETE", `/api/pages/${id}`);
  },
  reorder: (order) => http("PUT", "/api/pages/order", order),
  clear: () => http("DELETE", "/api/pages"),
  async save(target, nameTemplate, opts) {
    const res = await request("POST", "/api/save", { destination: target || null, name_template: nameTemplate, ...opts });
    if (target) return (await res.json()).files;
    const name = attachmentName(res.headers.get("content-disposition"));
    download(await res.blob(), name);
    return [name];
  },
  estimate: (opts) => http("POST", "/api/estimate", opts),
  defaultDir: async () => null,
  destinations: () => http("GET", "/api/destinations"),
  onScan: async (cb) => on("scan", cb),
  onPages: (cb) => on("pages", cb),
  onScanning: (cb) => on<{ scanning: boolean }>("state", (s) => cb(s.scanning)),
  onExpired: (cb) => on<{ minutes: number }>("expired", (e) => cb(e.minutes)),
};

export const api: Api = isWeb ? web : desktop;

export function errorText(e: unknown): string {
  return typeof e === "string" ? e : e instanceof Error ? e.message : JSON.stringify(e);
}

export function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(0)} KB`;
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
}
