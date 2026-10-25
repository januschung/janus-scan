# Showcase recordings

The animated pictures in the main README (`docs/screenshots/showcase-*.webp`) come from these scripts: the real server and web app, a stand-in scanner that "scans" made-up sample documents, and headless Chrome. Needs Google Chrome, Node, Rust and `img2webp` (`brew install webp`).

```sh
cd scripts/showcase && npm install
node pages.mjs /tmp/demo-pages                      # made-up documents, Letter at 300 dpi
(cd ../../app && npm run build:web)                 # the web app the demo serves
cargo run -p janus-scan-server --example demo -- /tmp/demo-pages 8090 &
node record.mjs desktop /tmp/frames-desktop         # restart the demo server before each recording
node assemble.mjs /tmp/frames-desktop ../../docs/screenshots/showcase-desktop.webp
node record.mjs phone /tmp/frames-phone
node assemble.mjs /tmp/frames-phone ../../docs/screenshots/showcase-phone.webp
```

Restart the demo server between recordings: each recording expects an empty document and the first pass of pages. `DEMO_PAGE_MS` sets how long each page takes to "scan" (default 900 ms; the README recordings use 600). Captions and steps are in `record.mjs`.
