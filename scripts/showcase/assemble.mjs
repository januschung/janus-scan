// Turns recorded frames into an animated WebP with the real frame timings.
//
//   node assemble.mjs <frames-dir> <out.webp> [quality] [fps]
import { readFileSync } from "node:fs";
import { execFileSync } from "node:child_process";

const [dir, out, q = "60", fps = "12"] = process.argv.slice(2);
const all = JSON.parse(readFileSync(`${dir}/frames.json`, "utf8"));
// Keep about `fps` frames a second; a kept frame lasts until the next kept one.
const frames = [];
for (const f of all) {
  const last = frames[frames.length - 1];
  if (last && last.ms < 1000 / Number(fps)) last.ms += f.ms;
  else frames.push({ ...f });
}
const args = ["-loop", "0", "-lossy", "-q", q, "-m", "4"];
for (const f of frames) args.push("-d", String(Math.max(40, f.ms)), f.path);
args.push("-o", out);
execFileSync("img2webp", args, { stdio: ["ignore", "ignore", "inherit"] });
console.log(`${out}: ${frames.length} frames`);
