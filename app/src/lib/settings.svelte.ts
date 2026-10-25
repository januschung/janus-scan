// User settings, remembered between launches in the webview's local storage.
import type { ColorMode, Format, PaperKind } from "./api";

const KEY = "janus-scan.settings.v1";

export interface Settings {
  deviceId: string;
  duplex: boolean;
  color: ColorMode;
  dpi: number;
  paper: PaperKind;
  multi: boolean;
  skipBlank: boolean;
  blankSensitivity: number;
  autoCrop: boolean;
  deskew: boolean;
  format: Format;
  jpegQuality: number;
  nameTemplate: string;
  dir: string;
  /** Web only: a NAS destination's name, or "" to download. */
  destination: string;
}

const defaults: Settings = {
  deviceId: "",
  duplex: true,
  color: "color",
  dpi: 300,
  paper: "letter",
  multi: true,
  skipBlank: true,
  blankSensitivity: 0.5,
  autoCrop: false,
  deskew: true,
  format: "pdf",
  jpegQuality: 85,
  nameTemplate: "Scan {date} {time}",
  dir: "",
  destination: "",
};

function load(): Settings {
  try {
    const raw = localStorage.getItem(KEY);
    return raw ? { ...defaults, ...JSON.parse(raw) } : { ...defaults };
  } catch {
    return { ...defaults };
  }
}

export const settings: Settings = $state(load());

export function persist() {
  try {
    localStorage.setItem(KEY, JSON.stringify(settings));
  } catch {
    // Storage can be unavailable; settings just won't be remembered.
  }
}
