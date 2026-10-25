// Records the README showcase from the demo server
// (crates/server/examples/demo.rs) as frames, with a caption for each step and
// a dot showing the pointer or finger. Assemble with make.sh.
//
//   node record.mjs <desktop|phone> <frames-dir> [url]
import { mkdirSync, rmSync, writeFileSync } from "node:fs";
import { chromium } from "playwright-core";

const [scene = "desktop", dir = "frames", url = "http://127.0.0.1:8090/"] = process.argv.slice(2);
const phone = scene === "phone";
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

rmSync(dir, { recursive: true, force: true });
mkdirSync(dir, { recursive: true });

const browser = await chromium.launch({ channel: "chrome", headless: true });
const ctx = await browser.newContext(
  phone
    ? { viewport: { width: 390, height: 844 }, isMobile: true, hasTouch: true, deviceScaleFactor: 1.5 }
    : { viewport: { width: 1280, height: 800 }, deviceScaleFactor: 1 },
);
await ctx.addInitScript((isPhone) => {
  localStorage.setItem(
    "janus-scan.settings.v1",
    JSON.stringify({ duplex: true, skipBlank: true, multi: true, dpi: 300, color: "color", format: "pdf", destination: "" }),
  );
  addEventListener("DOMContentLoaded", () => {
    const style = document.createElement("style");
    style.textContent = `
      #demo-caption { position: fixed; z-index: 99; left: 50%; transform: translateX(-50%); top: ${isPhone ? 66 : 70}px;
        max-width: ${isPhone ? 360 : 900}px; width: max-content; padding: ${isPhone ? "10px 16px" : "14px 26px"}; border-radius: 14px;
        background: rgb(20 24 38 / 88%); color: #fff; font: 700 ${isPhone ? 17 : 26}px/1.3 -apple-system, "Segoe UI", sans-serif;
        text-align: center; box-shadow: 0 10px 30px rgb(0 0 0 / 25%); pointer-events: none; transition: opacity .25s; }
      #demo-caption:empty { opacity: 0; }
      #demo-caption small { display: block; font-weight: 600; font-size: ${isPhone ? 12 : 15}px; opacity: .7; margin-bottom: 2px; letter-spacing: .04em; }
      #demo-dot { position: fixed; z-index: 100; width: ${isPhone ? 44 : 22}px; height: ${isPhone ? 44 : 22}px; margin: ${isPhone ? -22 : -11}px 0 0 ${isPhone ? -22 : -11}px;
        border-radius: 50%; background: rgb(61 99 240 / 25%); border: 2px solid rgb(61 99 240 / 85%); pointer-events: none;
        opacity: 0; transition: opacity .15s, transform .15s; }
      #demo-dot.down { transform: scale(.75); background: rgb(61 99 240 / 50%); }`;
    document.head.append(style);
    const caption = Object.assign(document.createElement("div"), { id: "demo-caption" });
    const dot = Object.assign(document.createElement("div"), { id: "demo-dot" });
    document.body.append(caption, dot);
    const at = (e) => { dot.style.left = `${e.clientX}px`; dot.style.top = `${e.clientY}px`; dot.style.opacity = "1"; };
    addEventListener("pointermove", at, true);
    addEventListener("pointerdown", (e) => { at(e); dot.classList.add("down"); }, true);
    addEventListener("pointerup", () => { dot.classList.remove("down"); if (isPhone) dot.style.opacity = "0"; }, true);
  });
}, phone);

const page = await ctx.newPage();
await page.goto(url);
await page.waitForSelector("#demo-caption", { state: "attached" });

// ---- frame capture: as fast as screenshots allow, with real timings
let recording = true;
const frames = [];
const capture = (async () => {
  let last = Date.now();
  while (recording) {
    const path = `${dir}/${String(frames.length).padStart(4, "0")}.jpg`;
    await page.screenshot({ path, type: "jpeg", quality: 85 });
    const now = Date.now();
    if (frames.length) frames[frames.length - 1].ms = now - last;
    frames.push({ path, ms: 100 });
    last = now;
  }
})();

const caption = (step, text) =>
  page.evaluate(([s, t]) => {
    document.getElementById("demo-caption").innerHTML = t ? (s ? `<small>${s}</small>` : "") + t : "";
  }, [step, text]);
const center = async (loc) => { const b = await loc.boundingBox(); return { x: b.x + b.width / 2, y: b.y + b.height / 2 }; };
const cards = page.locator("li.card");

// ---- input helpers
let mouse = { x: 640, y: 400 };
async function moveTo(p, steps = 18) { await page.mouse.move(p.x, p.y, { steps }); mouse = p; }
async function click(loc) { await moveTo(await center(loc)); await sleep(150); await page.mouse.down(); await sleep(90); await page.mouse.up(); }
const cdp = phone ? await ctx.newCDPSession(page) : null;
const touch = (type, p) => cdp.send("Input.dispatchTouchEvent", { type, touchPoints: type === "touchEnd" ? [] : [p] });
async function tap(loc) { const p = await center(loc); await touch("touchStart", p); await sleep(120); await touch("touchEnd"); }
async function waitPages(n) { await page.waitForFunction((k) => document.querySelectorAll("li.card").length >= k, n, { timeout: 60000 }); }

if (!phone) {
  await caption("", "Janus Scan for the Brother ADS-1000W");
  await sleep(2500);

  await caption("1 / 5", "Scan both sides in one pass · blank backs are skipped");
  await click(page.getByRole("button", { name: /^Scan$/ }));
  await page.getByText("More pages to feed?").waitFor({ timeout: 60000 });
  await sleep(1200);

  await caption("2 / 5", "Feeder empty? Add more pages to the same document");
  await sleep(1200);
  await click(page.getByRole("dialog").getByRole("button", { name: "Scan more" }));
  await page.getByText("More pages to feed?").waitFor({ timeout: 60000 });
  await sleep(900);
  await click(page.getByRole("button", { name: "Finish" }));
  await sleep(900);

  await caption("3 / 5", "A page went in upside down? Rotate it");
  await click(cards.nth(2).locator("button.thumb"));
  await sleep(1300);
  for (let i = 0; i < 2; i++) { await click(page.getByTitle("Rotate right (R)")); await sleep(900); }
  await sleep(800);

  await caption("4 / 5", "Drag pages into order");
  const from = await center(cards.nth(6).locator("button.thumb"));
  const to = await center(cards.nth(0).locator("button.thumb"));
  await moveTo(from);
  await page.mouse.down();
  await moveTo({ x: to.x - 60, y: to.y }, 45);
  await sleep(600);
  await page.mouse.up();
  await sleep(1500);

  await caption("5 / 5", "Save as PDF to a NAS folder · or download it");
  const dest = page.getByLabel("Save to");
  await dest.scrollIntoViewIfNeeded();
  await moveTo(await center(dest));
  await sleep(500);
  await dest.selectOption("Receipts");
  await sleep(1300);
  await click(page.getByTitle(/^Save/));
  await page.locator(".toast.ok").waitFor();
  await sleep(2500);
} else {
  await caption("", "Scan from your phone · just a web page, no app store");
  await sleep(2500);

  await caption("1 / 4", "One big Scan button · pages appear as they're scanned");
  await tap(page.getByRole("button", { name: /^Scan$/ }));
  await page.getByText("More pages to feed?").waitFor({ timeout: 60000 });
  await sleep(1000);
  await tap(page.getByRole("button", { name: "Finish" }));
  await sleep(1000);

  await caption("2 / 4", "Long-press, then drag to reorder");
  const a = await center(cards.nth(3).locator("button.thumb"));
  const b = await center(cards.nth(0).locator("button.thumb"));
  await touch("touchStart", a);
  await sleep(500);
  for (let i = 1; i <= 30; i++) { await touch("touchMove", { x: a.x + (b.x - 40 - a.x) * i / 30, y: a.y + (b.y - a.y) * i / 30 }); await sleep(30); }
  await sleep(500);
  await touch("touchEnd");
  await sleep(1500);

  await caption("3 / 4", "Tap a page to see it full screen and rotate it");
  // After the reorder, the upside-down page (Meeting Notes) is fourth.
  await tap(cards.nth(3).locator("button.thumb"));
  await sleep(1500);
  for (let i = 0; i < 2; i++) { await tap(page.getByTitle("Rotate right (R)")); await sleep(900); }
  await sleep(600);
  await tap(page.getByTitle("Close preview"));
  await sleep(900);

  await caption("4 / 4", "Save to a NAS folder or download to the phone");
  await tap(page.getByRole("button", { name: "Scan settings" }));
  await sleep(900);
  const dest = page.getByLabel("Save to");
  await dest.scrollIntoViewIfNeeded();
  await sleep(600);
  await dest.selectOption("Receipts");
  await sleep(1200);
  // The sheet covers most of the backdrop; tap the strip above it.
  await touch("touchStart", { x: 195, y: 130 }); await sleep(120); await touch("touchEnd");
  await sleep(700);
  await tap(page.getByTitle(/^Save/));
  await page.locator(".toast.ok").waitFor();
  await sleep(2200);

  await caption("", "Add it to your home screen and it opens like an app");
  await sleep(2600);
}

recording = false;
await capture;
writeFileSync(`${dir}/frames.json`, JSON.stringify(frames));
await browser.close();
console.log(`${scene}: ${frames.length} frames, ${(frames.reduce((s, f) => s + f.ms, 0) / 1000).toFixed(1)} s`);
