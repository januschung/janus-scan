// Made-up sample documents for the demo scanner (crates/server/examples/demo.rs):
// Letter pages at 300 dpi, one folder per feeder pass, backs included.
// Every name and number on them is invented.
//
//   node pages.mjs <out-dir>
import { mkdirSync } from "node:fs";
import { chromium } from "playwright-core";

const out = process.argv[2] ?? "demo-pages";
const ORG = "Maple Street Hardware";

const para = (n) =>
  Array.from({ length: n }, () =>
    `<p>Thank you for choosing ${ORG}. This document is a sample created to demonstrate scanning; every name and number on it is made up. Please keep this page with your records.</p>`).join("");

const table = (head, rows) =>
  `<table><tr>${head.map((h) => `<th>${h}</th>`).join("")}</tr>${rows
    .map((r) => `<tr>${r.map((c) => `<td>${c}</td>`).join("")}</tr>`)
    .join("")}</table>`;

const lines = (labels) => labels.map((l) => `<div class="line"><span>${l}</span><i></i></div>`).join("");

const docs = {
  invoice: ["INVOICE", para(1) + table(["Item", "Qty", "Price"], [["Garden hose, 15 m", 1, "$24.99"], ["Brass nozzle", 2, "$9.50"], ["Seed mix, wildflower", 3, "$4.25"], ["Gloves, size M", 1, "$12.00"]]) + `<div class="total">Total&nbsp; $64.74</div>` + para(1)],
  welcome: ["Welcome to the neighbourhood!", para(4) + `<p class="sign">The Oak Lane Neighbours</p>`],
  notes: ["Meeting Notes", `<h2>Garden club · October</h2>` + para(2) + `<ul><li>Bulb swap on the 26th</li><li>New compost bins arrive Friday</li><li>Volunteers needed for the spring fair</li></ul>` + para(1)],
  warranty: ["Warranty Card", `<div class="card">${lines(["Model: WS-200 Windbreaker", "Purchased: May 12", "Coverage: 2 years, parts and labour"])}</div>` + para(1)],
  terms: ["Warranty Terms", para(5)],
  meals: ["Weekly Meal Plan", table(["Day", "Lunch", "Dinner"], [["Monday", "Lentil soup", "Fish tacos"], ["Tuesday", "Rice bowls", "Pasta bake"], ["Wednesday", "Wraps", "Stir-fry"], ["Thursday", "Salad", "Curry"], ["Friday", "Leftovers", "Pizza night"]]) + para(1)],
  trip: ["Field Trip Permission Slip", para(1) + `<div class="card">${lines(["Student name", "Class", "Parent or guardian", "Phone", "Signature"])}</div>` + para(1)],
};

const page = (key, n, { upsideDown = false } = {}) => {
  if (key === "blank") return `<div class="paper blank"></div>`;
  const [title, body] = docs[key];
  return `<div class="paper"><div class="content" style="${upsideDown ? "transform: rotate(180deg)" : ""}">
    <h1>${title}</h1><div class="sub">Sample document ${n} · ${ORG} · Janus Scan demo</div>${body}
    <div class="foot">Page ${n}</div></div></div>`;
};

// Pass 1 is two-sided: front, back, front, back, ...
const passes = {
  "pass-1": [page("invoice", 1), page("blank"), page("welcome", 2), page("blank"), page("notes", 3, { upsideDown: true }), page("blank"), page("warranty", 4), page("terms", 5)],
  "pass-2": [page("meals", 6), page("blank"), page("trip", 7), page("blank")],
};

const css = `
  * { box-sizing: border-box; margin: 0; }
  body { width: 850px; height: 1100px; }
  .paper { width: 850px; height: 1100px; background: #fbfaf6; font: 15px/1.6 -apple-system, "Helvetica Neue", Arial, sans-serif; color: #222; }
  .paper.blank { background: #fdfdfb; }
  .content { height: 100%; padding: 80px 72px; position: relative; }
  h1 { font: 700 30px Georgia, serif; letter-spacing: .02em; }
  h2 { font-size: 17px; margin: 18px 0 6px; }
  .sub { color: #666; font-size: 12px; padding: 6px 0 10px; border-bottom: 2px solid #3d63f0; margin-bottom: 24px; }
  p { margin: 0 0 16px; }
  ul { margin: 0 0 16px 22px; }
  table { width: 100%; border-collapse: collapse; margin: 8px 0 16px; }
  th { text-align: left; background: #e8edff; padding: 8px 10px; font-size: 13px; }
  td { padding: 9px 10px; border-bottom: 1px solid #e3e6eb; }
  .total { text-align: right; font-weight: 700; font-size: 17px; margin: 0 10px 24px; }
  .sign { font-style: italic; margin-top: 30px; }
  .card { border: 1.5px solid #c9ced8; border-radius: 8px; padding: 22px 26px 8px; margin: 8px 0 24px; }
  .line { display: flex; gap: 12px; margin-bottom: 18px; font-size: 14px; }
  .line span { width: 210px; color: #555; } .line i { flex: 1; border-bottom: 1px solid #999; }
  .foot { position: absolute; bottom: 48px; left: 0; right: 0; text-align: center; color: #888; font-size: 11px; }`;

const browser = await chromium.launch({ channel: "chrome", headless: true });
const ctx = await browser.newContext({ viewport: { width: 850, height: 1100 }, deviceScaleFactor: 3 }); // 2550 x 3300
const tab = await ctx.newPage();
for (const [pass, pages] of Object.entries(passes)) {
  mkdirSync(`${out}/${pass}`, { recursive: true });
  for (const [i, html] of pages.entries()) {
    await tab.setContent(`<style>${css}</style>${html}`);
    await tab.screenshot({ path: `${out}/${pass}/${String(i + 1).padStart(2, "0")}.png` });
  }
}
await browser.close();
console.log(`wrote ${out}/pass-1 (${passes["pass-1"].length} sides), ${out}/pass-2 (${passes["pass-2"].length} sides)`);
