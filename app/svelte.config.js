// Tauri doesn't have a Node.js server to do proper SSR
// so we use adapter-static with a fallback to index.html to put the site in SPA mode
// See: https://svelte.dev/docs/kit/single-page-apps
// See: https://v2.tauri.app/start/frontend/sveltekit/ for more info
import adapter from "@sveltejs/adapter-static";
import process from "node:process";

// `VITE_JANUS_WEB=1` builds the web front end for janus-scan-server into
// build-web/; otherwise the desktop app's front end goes to build/.
const web = process.env.VITE_JANUS_WEB === "1";
import { vitePreprocess } from "@sveltejs/vite-plugin-svelte";

/** @type {import('@sveltejs/kit').Config} */
const config = {
  preprocess: vitePreprocess(),
  kit: {
    adapter: adapter({
      pages: web ? "build-web" : "build",
      assets: web ? "build-web" : "build",
      fallback: "index.html",
    }),
  },
};

export default config;
