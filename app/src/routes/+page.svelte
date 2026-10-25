<script lang="ts">
  import { onMount } from "svelte";
  import { open as openDialog } from "@tauri-apps/plugin-dialog";
  import { revealItemInDir } from "@tauri-apps/plugin-opener";
  import {
    api,
    isWeb,
    errorText,
    formatBytes,
    type Capabilities,
    type DeviceInfo,
    type Destination,
    type ExportOptions,
    type PageView,
    type ScanUpdate,
  } from "$lib/api";
  import { settings, persist } from "$lib/settings.svelte";
  import Icon from "$lib/components/Icon.svelte";
  import Segmented from "$lib/components/Segmented.svelte";
  import Toggle from "$lib/components/Toggle.svelte";
  import PageGrid, { type Pick } from "$lib/components/PageGrid.svelte";

  type Status = "searching" | "opening" | "ready" | "scanning" | "saving" | "no-device";
  type Toast = { kind: "error" | "ok" | "info"; text: string; path?: string };

  const PAPERS = [
    { value: "letter", label: "Letter" },
    { value: "legal", label: "Legal" },
    { value: "a4", label: "A4" },
    { value: "a5", label: "A5" },
    { value: "business-card", label: "Business card" },
    { value: "receipt", label: "Receipt" },
    { value: "auto", label: "Full area" },
  ] as const;

  let devices: DeviceInfo[] = $state([]);
  let caps = $state<Capabilities | null>(null);
  let status = $state<Status>("searching");
  let pages: PageView[] = $state([]);
  let selected = $state<number | null>(null);
  let picked = $state<number[]>([]);
  let anchor: number | null = null;
  let preview = $state<string | null>(null);
  let askMore = $state(false);
  let passPages = $state(0);
  let skipped = $state(0);
  let toast = $state<Toast | null>(null);
  let estimate = $state<number | null>(null);
  let unsaved = $state(false);
  let destinations: Destination[] = $state([]);
  // Phone layout: settings live in a bottom sheet, the preview opens full screen.
  let phone = $state(false);
  let sheetOpen = $state(false);
  // This window started the pass in progress. With the shared server document,
  // other browsers see the pages arrive but don't get the "More pages?" prompt.
  let mine = false;

  const device = $derived(devices.find((d) => d.id === settings.deviceId) ?? null);
  const busy = $derived(status === "scanning" || status === "saving" || status === "opening");
  // Web: where Save puts the document; null downloads it.
  const dest = $derived(destinations.find((d) => d.name === settings.destination) ?? null);
  const selectedIndex = $derived(pages.findIndex((p) => p.id === selected));
  const exportOpts = $derived<ExportOptions>({
    format: settings.format,
    jpeg_quality: settings.jpegQuality,
    multi_page_tiff: true,
    pdf_title: null,
  });

  $effect(() => {
    JSON.stringify(settings);
    persist();
  });

  // ---- Devices ------------------------------------------------------------

  async function refreshDevices(quiet = false) {
    if (!quiet) status = "searching";
    try {
      const list = await api.listDevices();
      devices = list;
      const keep = list.find((d) => d.id === settings.deviceId) ?? list[0];
      if (!keep) {
        caps = null;
        status = "no-device";
        return;
      }
      if (keep.id !== settings.deviceId || !caps) await selectDevice(keep.id);
      else if (!quiet) status = "ready";
    } catch (e) {
      status = "no-device";
      showError(e);
    }
  }

  async function selectDevice(id: string) {
    settings.deviceId = id;
    status = "opening";
    try {
      caps = await api.openDevice(id);
      if (caps.resolutions.length && !caps.resolutions.includes(settings.dpi)) {
        settings.dpi = caps.resolutions.reduce((a, b) => (Math.abs(b - 300) < Math.abs(a - 300) ? b : a));
      }
      if (!caps.duplex) settings.duplex = false;
      status = "ready";
    } catch (e) {
      caps = null;
      status = "no-device";
      showError(e);
    }
  }

  // ---- Scanning -----------------------------------------------------------

  async function scan() {
    if (!caps || busy) return;
    askMore = false;
    sheetOpen = false;
    passPages = 0;
    status = "scanning";
    mine = true;
    try {
      await api.startScan({
        request: {
          source: caps.sources.includes("feeder") ? "feeder" : "flatbed",
          duplex: settings.duplex && caps.duplex,
          dpi: settings.dpi,
          color: settings.color,
          paper: { kind: settings.paper },
        },
        skip_blank: settings.skipBlank,
        blank_sensitivity: settings.blankSensitivity,
        auto_crop: settings.autoCrop,
        deskew: settings.deskew,
      });
    } catch (e) {
      status = "ready";
      mine = false;
      showError(e);
    }
  }

  function onScan(u: ScanUpdate) {
    switch (u.kind) {
      case "page":
        if (pages.some((p) => p.id === u.page.id)) break;
        pages.push(u.page);
        passPages += 1;
        unsaved = true;
        if (selected === null && !phone) select(u.page.id);
        break;
      case "skipped":
        skipped += 1;
        break;
      case "feeder-empty":
        status = "ready";
        if (settings.multi && mine) askMore = true;
        mine = false;
        break;
      case "done":
        status = "ready";
        mine = false;
        break;
      case "error":
        status = "ready";
        if (u.error.kind === "cancelled") toast = { kind: "info", text: "Scan cancelled." };
        else if ((u.error.kind === "no-paper" || u.error.kind === "not-started") && askMore === false && pages.length && settings.multi && mine) askMore = true;
        else showError(u.message);
        mine = false;
        break;
    }
  }

  function onPages(list: PageView[]) {
    pages = list;
    const ids = new Set(list.map((p) => p.id));
    picked = picked.filter((id) => ids.has(id));
    if (selected !== null && !ids.has(selected)) select(null);
    else if (selected !== null) select(selected); // it may have been rotated
  }

  function onScanning(scanning: boolean) {
    if (scanning && status === "ready") {
      status = "scanning";
      passPages = 0;
    } else if (!scanning && status === "scanning") {
      status = "ready";
    }
  }

  // ---- Pages --------------------------------------------------------------

  async function select(id: number | null) {
    selected = id;
    preview = null;
    if (id === null) return;
    try {
      const url = await api.preview(id, 1800);
      if (selected === id) preview = url;
    } catch (e) {
      showError(e);
    }
  }

  async function rotate(id: number, clockwise: boolean) {
    try {
      const page = await api.rotate(id, clockwise);
      pages = pages.map((p) => (p.id === id ? page : p));
      unsaved = true;
      if (selected === id) select(id);
    } catch (e) {
      showError(e);
    }
  }

  function pick(id: number, how: Pick) {
    const at = (x: number) => pages.findIndex((p) => p.id === x);
    if (how.range && anchor !== null && at(anchor) !== -1) {
      const [a, b] = [at(anchor), at(id)].sort((x, y) => x - y);
      picked = pages.slice(a, b + 1).map((p) => p.id);
    } else if (how.toggle) {
      picked = picked.includes(id) ? picked.filter((x) => x !== id) : [...picked, id];
      anchor = id;
    } else {
      picked = [id];
      anchor = id;
    }
    select(id);
  }

  function selectAll() {
    picked = pages.map((p) => p.id);
    anchor = picked[0] ?? null;
  }

  function clearPicked() {
    picked = selected === null ? [] : [selected];
  }

  /** Removes pages from this document only; saved files stay on disk. */
  async function remove(ids: number[]) {
    if (!ids.length) return;
    if (ids.length > 1 && !confirm(`Remove ${ids.length} pages from this document?\n\nFiles you've already saved aren't affected.`)) return;
    try {
      await api.remove(ids);
      const first = pages.findIndex((p) => ids.includes(p.id));
      pages = pages.filter((p) => !ids.includes(p.id));
      picked = picked.filter((x) => !ids.includes(x));
      if (selected !== null && ids.includes(selected)) {
        const next = pages[Math.min(first, pages.length - 1)]?.id ?? null;
        select(next);
        picked = next === null ? [] : [next];
      }
    } catch (e) {
      showError(e);
      pages = await api.listPages();
    }
  }

  async function rotatePicked(clockwise: boolean) {
    for (const id of picked) await rotate(id, clockwise);
  }

  async function reorder(order: number[]) {
    const byId = new Map(pages.map((p) => [p.id, p]));
    pages = order.map((id) => byId.get(id)!).filter(Boolean);
    unsaved = true;
    try {
      await api.reorder(order);
    } catch (e) {
      showError(e);
      pages = await api.listPages();
    }
  }

  async function newDocument() {
    if (unsaved && pages.length && !confirm("Discard the unsaved pages?")) return;
    await api.clear();
    pages = [];
    picked = [];
    skipped = 0;
    unsaved = false;
    askMore = false;
    select(null);
  }

  function step(delta: number) {
    const next = pages[selectedIndex + delta];
    if (next) pick(next.id, { toggle: false, range: false });
  }

  // ---- Saving -------------------------------------------------------------

  async function chooseFolder() {
    const dir = await openDialog({ directory: true, defaultPath: settings.dir || undefined });
    if (typeof dir === "string") settings.dir = dir;
  }

  async function save() {
    if (!pages.length || busy) return;
    if (!isWeb && !settings.dir) {
      settings.dir = (await api.defaultDir()) ?? "";
      if (!settings.dir) return chooseFolder();
    }
    status = "saving";
    askMore = false;
    // Captured now: the picker may change while the save runs.
    const to = dest;
    try {
      const target = isWeb ? (to?.name ?? "") : settings.dir;
      const files = await api.save(target, to?.name_template ?? settings.nameTemplate, exportOpts);
      unsaved = false;
      const name = files[0]?.split(/[\\/]/).pop() ?? "";
      const what = files.length > 1 ? `${files.length} files` : name;
      toast = {
        kind: "ok",
        text: isWeb && !to ? `Downloaded ${what}` : to ? `Saved ${what} to ${to.name}` : `Saved ${what}`,
        path: isWeb ? undefined : files[0],
      };
    } catch (e) {
      showError(e);
    } finally {
      status = "ready";
    }
  }

  // Size hint, debounced; one page is encoded and extrapolated on the Rust side.
  let estimateTimer: ReturnType<typeof setTimeout>;
  $effect(() => {
    const opts = exportOpts;
    const count = pages.length;
    clearTimeout(estimateTimer);
    if (!count) return void (estimate = null);
    estimateTimer = setTimeout(async () => {
      try {
        estimate = await api.estimate(opts);
      } catch {
        estimate = null;
      }
    }, 400);
  });

  // ---- Misc ---------------------------------------------------------------

  let toastTimer: ReturnType<typeof setTimeout>;
  function showError(e: unknown) {
    toast = { kind: "error", text: errorText(e) };
  }
  $effect(() => {
    if (!toast) return;
    clearTimeout(toastTimer);
    toastTimer = setTimeout(() => (toast = null), toast.kind === "error" ? 8000 : 5000);
  });

  function onKey(e: KeyboardEvent) {
    const typing = (e.target as HTMLElement)?.closest("input, select, textarea");
    const mod = e.metaKey || e.ctrlKey;
    if (mod && e.key.toLowerCase() === "s") {
      e.preventDefault();
      save();
    } else if (typing) {
      return;
    } else if (mod && e.key.toLowerCase() === "a") {
      e.preventDefault();
      selectAll();
    } else if (askMore && e.key === "Escape") {
      askMore = false;
    } else if (e.key === "Escape" && picked.length > 1) {
      clearPicked();
    } else if ((e.key === "Enter" || e.key === " ") && !(e.target as HTMLElement)?.closest("button")) {
      e.preventDefault();
      if (status === "scanning") return;
      scan();
    } else if (status === "scanning" && e.key === "Escape") {
      api.cancelScan();
    } else if (selected !== null) {
      if (e.key === "Delete" || e.key === "Backspace") remove(picked.length ? picked : [selected]);
      else if (e.key === "r") rotate(selected, true);
      else if (e.key === "R") rotate(selected, false);
      else if (e.key === "ArrowRight" || e.key === "ArrowDown") step(1);
      else if (e.key === "ArrowLeft" || e.key === "ArrowUp") step(-1);
    }
  }

  onMount(() => {
    const unlisten = api.onScan(onScan);
    const unPages = api.onPages(onPages);
    const unScanning = api.onScanning(onScanning);
    const unExpired = api.onExpired((minutes) => {
      unsaved = false;
      askMore = false;
      skipped = 0;
      const [n, unit] = minutes >= 60 && minutes % 60 === 0 ? [minutes / 60, "hour"] : [minutes, "minute"];
      const after = `${n} ${unit}${n === 1 ? "" : "s"}`;
      toast = { kind: "info", text: `Unsaved pages were removed after ${after} without activity.` };
    });
    const mq = matchMedia("(max-width: 700px)");
    const onMq = () => (phone = mq.matches);
    onMq();
    mq.addEventListener("change", onMq);
    api.listPages().then((p) => (pages = p));
    api.destinations().then((d) => (destinations = d), showError);
    refreshDevices();
    // Hot-plug and Wi-Fi discovery: poll quietly while idle.
    const poll = setInterval(() => {
      if (status === "ready" || status === "no-device") refreshDevices(true);
    }, 5000);
    return () => {
      clearInterval(poll);
      mq.removeEventListener("change", onMq);
      unPages();
      unScanning();
      unExpired();
      unlisten.then((f) => f());
    };
  });
</script>

<svelte:window onkeydown={onKey} />

<div class="app">
  <header>
    <div class="brand">
      <span class="logo" aria-hidden="true">
        <!-- Same mark as icons/source/janus-scan.svg, without the eyes at this size. -->
        <svg viewBox="0 0 100 100" width="22" height="22">
          <path d="M46.5 18 A32 32 0 0 0 46.5 82 Z M17.2 44.5 L8.5 55.5 L18.6 58.6 Z" fill="#fff" />
          <path d="M53.5 18 A32 32 0 0 1 53.5 82 Z M82.8 44.5 L91.5 55.5 L81.4 58.6 Z" fill="#fff" opacity="0.72" />
        </svg>
      </span>
      <span class="brand-name">Janus Scan</span>
    </div>

    <div class="device">
      <span
        class="dot"
        class:live={status !== "no-device" && status !== "searching"}
        class:warn={status === "no-device"}
        title={status}
      ></span>
      {#if devices.length}
        <select
          class="field device-select"
          value={settings.deviceId}
          disabled={busy}
          onchange={(e) => selectDevice((e.target as HTMLSelectElement).value)}
          aria-label="Scanner"
        >
          {#each devices as d (d.id)}
            <option value={d.id}>{d.name} · {d.connection === "usb" ? "USB" : "Wi-Fi"}</option>
          {/each}
        </select>
        {#if device}
          <span class="conn" title={device.connection === "usb" ? "Connected by USB" : "Connected over Wi-Fi"}>
            <Icon name={device.connection === "usb" ? "usb" : "wifi"} size={16} />
          </span>
        {/if}
      {:else}
        <span class="muted">{status === "searching" ? "Looking for scanners…" : "No scanner found"}</span>
      {/if}
      <button class="btn ghost icon" title="Look for scanners" onclick={() => refreshDevices()} disabled={busy}>
        <Icon name="refresh" size={16} />
      </button>
    </div>

    <div class="spacer"></div>

    {#if pages.length}
      <span class="muted count">
        {pages.length} page{pages.length === 1 ? "" : "s"}{#if skipped}&nbsp;· {skipped} blank skipped{/if}
      </span>
    {/if}
    <button class="btn" onclick={newDocument} disabled={busy || !pages.length}>
      <Icon name="plus" size={16} /> <span class="btn-label">New</span>
    </button>
    <button class="btn primary" onclick={save} disabled={busy || !pages.length} title="Save (⌘S / Ctrl+S)">
      <Icon name="save" size={16} /> <span class="btn-label">Save {settings.format.toUpperCase()}</span>
    </button>
  </header>

  <div class="body">
    {#if sheetOpen}
      <button class="sheet-scrim" aria-label="Close settings" onclick={() => (sheetOpen = false)}></button>
    {/if}
    <aside class:open={sheetOpen}>
      <div class="scroll">
        <section>
          <h3>Scan</h3>
          <div class="label">Sides</div>
          <Segmented
            label="Sides"
            bind:value={settings.duplex}
            options={[
              { value: false, label: "One side" },
              { value: true, label: "Both sides", disabled: caps ? !caps.duplex : false },
            ]}
          />
          <div class="label">Color</div>
          <Segmented
            label="Color"
            bind:value={settings.color}
            options={[
              { value: "color", label: "Color" },
              { value: "gray", label: "Gray" },
              { value: "bw", label: "B&W" },
            ]}
          />
          <div class="two">
            <label>
              <span class="label">Resolution</span>
              <select class="field" bind:value={settings.dpi}>
                {#each caps?.resolutions ?? [settings.dpi] as r (r)}
                  <option value={r}>{r} dpi</option>
                {/each}
              </select>
            </label>
            <label>
              <span class="label">Paper</span>
              <select class="field" bind:value={settings.paper}>
                {#each PAPERS as p (p.value)}
                  <option value={p.value}>{p.label}</option>
                {/each}
              </select>
            </label>
          </div>
          <Toggle bind:checked={settings.multi} label="Multi-page mode" hint="Ask for more pages when the feeder empties" />
        </section>

        <section>
          <h3>Clean up</h3>
          <Toggle bind:checked={settings.skipBlank} label="Skip blank pages" hint="Drops empty backs of one-sided pages" />
          {#if settings.skipBlank}
            <label class="slider">
              <span class="label">Blank sensitivity</span>
              <input type="range" min="0" max="1" step="0.05" bind:value={settings.blankSensitivity} />
            </label>
          {/if}
          <Toggle bind:checked={settings.deskew} label="Straighten pages" />
          <Toggle bind:checked={settings.autoCrop} label="Crop to page" hint="Trims scanner background" />
        </section>

        <section>
          <h3>Save as</h3>
          <Segmented
            label="File type"
            bind:value={settings.format}
            options={[
              { value: "pdf", label: "PDF" },
              { value: "jpeg", label: "JPEG" },
              { value: "png", label: "PNG" },
              { value: "tiff", label: "TIFF" },
            ]}
          />
          {#if settings.format === "pdf" || settings.format === "jpeg"}
            <label class="slider">
              <span class="label">Quality <span class="muted">{settings.jpegQuality}</span></span>
              <input type="range" min="40" max="95" step="5" bind:value={settings.jpegQuality} />
            </label>
          {/if}
          {#if isWeb}
            <label>
              <span class="label">Save to</span>
              <select class="field" bind:value={settings.destination}>
                <option value="">This device</option>
                {#each destinations as d (d.name)}
                  <option value={d.name}>{d.name}</option>
                {/each}
              </select>
            </label>
          {/if}
          {#if dest?.name_template}
            <label>
              <span class="label">File name</span>
              <input class="field" value={dest.name_template} disabled />
            </label>
            <div class="hint">Set for the {dest.name} folder.</div>
          {:else}
            <label>
              <span class="label">File name</span>
              <input class="field" bind:value={settings.nameTemplate} spellcheck="false" />
            </label>
            <div class="hint">Use {"{date}"}, {"{time}"} and {"{n}"}.</div>
          {/if}
          {#if !isWeb}
            <span class="label">Folder</span>
            <button class="btn folder" onclick={chooseFolder}>
              <Icon name="folder" size={16} />
              <span class="path">{settings.dir ? settings.dir.split(/[\\/]/).pop() : "Documents"}</span>
            </button>
          {/if}
          {#if estimate !== null}
            <div class="hint">About {formatBytes(estimate)}</div>
          {/if}
        </section>
      </div>

      <div class="scanbar">
        {#if status === "scanning"}
          <div class="progress"><span class="spinner"></span> Scanning… {passPages ? `page ${passPages}` : ""}</div>
          <button class="btn lg scancel" onclick={() => api.cancelScan()}>Cancel</button>
        {:else}
          <div class="scan-row">
            <button
              class="btn lg settings-toggle"
              onclick={() => (sheetOpen = !sheetOpen)}
              aria-expanded={sheetOpen}
              title="Scan settings"
            >
              <Icon name={sheetOpen ? "x" : "sliders"} size={18} />
            </button>
            <button class="btn primary lg" onclick={scan} disabled={!caps || busy}>
              <Icon name="scan" size={18} stroke={2.2} />
              {pages.length ? "Scan more" : "Scan"}
            </button>
          </div>
          <div class="hint center keys"><kbd>Space</kbd> to scan</div>
        {/if}
      </div>
    </aside>

    <main class:has-preview={selected !== null}>
      {#if pages.length}
        <div class="grid-wrap">
          <div class="selbar" class:active={picked.length > 1}>
            {#if picked.length > 1}
              <span class="selcount">{picked.length} of {pages.length} selected</span>
              <button class="btn ghost" onclick={() => rotatePicked(false)} title="Rotate selected left">
                <Icon name="rotate-ccw" size={16} />
              </button>
              <button class="btn ghost" onclick={() => rotatePicked(true)} title="Rotate selected right">
                <Icon name="rotate-cw" size={16} />
              </button>
              <button class="btn danger" onclick={() => remove(picked)} title="Remove from this document (saved files are kept)">
                <Icon name="trash" size={16} /> Remove from document
              </button>
              <div class="spacer"></div>
              {#if picked.length < pages.length}
                <button class="btn ghost" onclick={selectAll}>Select all</button>
              {/if}
              <button class="btn ghost" onclick={clearPicked}>Done</button>
            {:else}
              <span class="muted hint-inline keys">
                <kbd>{navigator.platform.startsWith("Mac") ? "⌘" : "Ctrl"}</kbd>-click or <kbd>Shift</kbd>-click to select several
              </span>
              <div class="spacer"></div>
              <button class="btn ghost" onclick={selectAll}>Select all</button>
            {/if}
          </div>
          <PageGrid
            {pages}
            {selected}
            {picked}
            onselect={pick}
            onrotate={rotate}
            ondelete={(id) => remove([id])}
            onreorder={reorder}
          />
        </div>
        {#if selected !== null}
          <div class="preview">
            <div class="preview-bar">
              <button class="btn ghost icon" onclick={() => step(-1)} disabled={selectedIndex <= 0} title="Previous">
                <Icon name="chevron-left" />
              </button>
              <span class="muted">Page {selectedIndex + 1} of {pages.length}</span>
              <button class="btn ghost icon" onclick={() => step(1)} disabled={selectedIndex >= pages.length - 1} title="Next">
                <Icon name="chevron-right" />
              </button>
              <div class="spacer"></div>
              <button class="btn ghost icon" title="Rotate left (Shift+R)" onclick={() => rotate(selected!, false)}>
                <Icon name="rotate-ccw" />
              </button>
              <button class="btn ghost icon" title="Rotate right (R)" onclick={() => rotate(selected!, true)}>
                <Icon name="rotate-cw" />
              </button>
              <button class="btn ghost icon danger" title="Remove from document (Delete)" onclick={() => remove([selected!])}>
                <Icon name="trash" />
              </button>
              <button class="btn ghost icon" title="Close preview" onclick={() => select(null)}>
                <Icon name="x" />
              </button>
            </div>
            <div class="preview-img">
              {#if preview}
                <img src={preview} alt={`Page ${selectedIndex + 1}`} />
              {:else}
                <span class="spinner big"></span>
              {/if}
            </div>
          </div>
        {/if}
      {:else}
        <div class="empty">
          <div class="art">
            <div class="sheet s1"></div>
            <div class="sheet s2"></div>
            <div class="sheet s3"></div>
          </div>
          <h2>
            {#if status === "no-device"}No scanner connected{:else if status === "scanning"}Scanning…{:else}Ready to scan{/if}
          </h2>
          <p class="muted">
            {#if status === "no-device"}
              Plug in the scanner or join it to Wi-Fi. It shows up here automatically.
            {:else}
              Load pages face down in the feeder, then press <b>Scan</b>.
            {/if}
          </p>
        </div>
      {/if}
    </main>
  </div>

  {#if askMore}
    <div class="scrim" role="presentation">
      <div class="dialog" role="dialog" aria-modal="true" aria-labelledby="more-title">
        <div class="dialog-icon"><Icon name="stack" size={22} /></div>
        <h2 id="more-title">More pages to feed?</h2>
        <p class="muted">
          {pages.length} page{pages.length === 1 ? "" : "s"} so far. Load the next batch and they'll be added to the end.
        </p>
        <div class="dialog-actions">
          <button class="btn lg" onclick={() => (askMore = false)}>Finish</button>
          <!-- svelte-ignore a11y_autofocus -->
          <button class="btn primary lg" onclick={scan} autofocus>Scan more</button>
        </div>
        <div class="hint center"><kbd>Enter</kbd> scan more · <kbd>Esc</kbd> finish</div>
      </div>
    </div>
  {/if}

  {#if toast}
    <div class="toast {toast.kind}" role="status">
      <Icon name={toast.kind === "error" ? "alert" : toast.kind === "ok" ? "check" : "file"} size={16} />
      <span>{toast.text}</span>
      {#if toast.path}
        <button class="link" onclick={() => toast?.path && revealItemInDir(toast.path)}>Show in folder</button>
      {/if}
      <button class="close" onclick={() => (toast = null)} aria-label="Dismiss"><Icon name="x" size={14} /></button>
    </div>
  {/if}
</div>

<style>
  .app {
    display: flex;
    flex-direction: column;
    height: 100vh;
  }
  header {
    display: flex;
    align-items: center;
    gap: 10px;
    height: 56px;
    padding: 0 16px;
    border-bottom: 1px solid var(--border);
    background: var(--surface);
  }
  .brand {
    display: flex;
    align-items: center;
    gap: 8px;
    font-weight: 700;
    font-size: 15px;
    letter-spacing: -0.01em;
    width: calc(var(--sidebar) - 16px);
  }
  .logo {
    display: grid;
    place-items: center;
    width: 28px;
    height: 28px;
    border-radius: 8px;
    background: var(--accent);
    color: var(--on-accent);
  }
  .device {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
  }
  .device-select {
    width: auto;
    max-width: 280px;
    font-weight: 500;
  }
  .dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--text-3);
    flex: none;
  }
  .dot.live {
    background: var(--ok);
    box-shadow: 0 0 0 3px color-mix(in srgb, var(--ok) 20%, transparent);
  }
  .dot.warn {
    background: var(--danger);
  }
  .conn {
    color: var(--text-2);
    display: grid;
  }
  .spacer {
    flex: 1;
  }
  .muted {
    color: var(--text-2);
  }
  .count {
    font-size: 13px;
    margin-right: 4px;
  }

  .body {
    flex: 1;
    display: flex;
    min-height: 0;
  }
  aside {
    width: var(--sidebar);
    flex: none;
    display: flex;
    flex-direction: column;
    border-right: 1px solid var(--border);
    background: var(--surface);
  }
  .scroll {
    flex: 1;
    overflow-y: auto;
    padding: 4px 18px 12px;
  }
  section {
    padding: 14px 0;
    border-bottom: 1px solid var(--border);
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  section:last-child {
    border-bottom: 0;
  }
  h3 {
    margin: 0 0 4px;
    font-size: 11px;
    font-weight: 700;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    color: var(--text-3);
  }
  .label {
    display: block;
    font-size: 12px;
    font-weight: 500;
    color: var(--text-2);
    margin: 6px 0 4px;
  }
  .two {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 10px;
  }
  .slider input {
    width: 100%;
    accent-color: var(--accent);
  }
  .hint {
    font-size: 12px;
    color: var(--text-3);
  }
  .center {
    text-align: center;
  }
  .folder {
    justify-content: flex-start;
    width: 100%;
  }
  .path {
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .scanbar {
    padding: 14px 18px 16px;
    border-top: 1px solid var(--border);
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .scan-row {
    display: flex;
    gap: 8px;
  }
  .scan-row .primary {
    flex: 1;
  }
  .settings-toggle,
  .sheet-scrim {
    display: none;
  }
  .scancel {
    width: 100%;
  }
  .progress {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 8px;
    height: 20px;
    color: var(--text-2);
    font-weight: 500;
  }

  main {
    flex: 1;
    min-width: 0;
    display: flex;
  }
  .grid-wrap {
    flex: 1;
    overflow-y: auto;
  }
  .selbar {
    position: sticky;
    top: 0;
    z-index: 2;
    display: flex;
    align-items: center;
    gap: 4px;
    height: 48px;
    padding: 0 12px 0 20px;
    background: color-mix(in srgb, var(--bg) 94%, transparent);
    border-bottom: 1px solid transparent;
    transition: background 0.15s, border-color 0.15s;
  }
  .selbar.active {
    background: var(--accent-soft);
    border-bottom-color: var(--border);
  }
  .selcount {
    font-weight: 600;
    margin-right: 8px;
  }
  .hint-inline {
    font-size: 12px;
  }
  .preview {
    width: 46%;
    min-width: 340px;
    display: flex;
    flex-direction: column;
    border-left: 1px solid var(--border);
    background: var(--surface-2);
    animation: slide 0.18s ease-out;
  }
  .preview-bar {
    display: flex;
    align-items: center;
    gap: 2px;
    padding: 8px 10px;
    border-bottom: 1px solid var(--border);
    background: var(--surface);
  }
  .preview-img {
    flex: 1;
    min-height: 0;
    display: grid;
    place-items: center;
    padding: 20px;
  }
  .preview-img img {
    max-width: 100%;
    max-height: 100%;
    object-fit: contain;
    background: #fff;
    box-shadow: var(--shadow-lg);
    border-radius: 3px;
  }
  @media (max-width: 980px) and (min-width: 701px) {
    .preview {
      display: none;
    }
  }

  /* Phones: settings in a bottom sheet, a big Scan button, preview full screen. */
  @media (max-width: 700px) {
    header {
      gap: 6px;
      padding: 0 10px;
    }
    .brand {
      width: auto;
    }
    .brand-name,
    .count,
    .conn,
    .keys,
    .btn-label {
      display: none;
    }
    .device-select {
      max-width: 38vw;
    }
    .body {
      display: block;
      overflow-y: auto;
      padding-bottom: calc(96px + env(safe-area-inset-bottom));
    }
    main {
      min-height: 100%;
    }
    .grid-wrap {
      overflow: visible;
    }
    aside {
      position: fixed;
      left: 0;
      right: 0;
      bottom: 0;
      z-index: 6;
      width: auto;
      max-height: 78vh;
      border-right: 0;
      border-top: 1px solid var(--border);
      border-radius: 16px 16px 0 0;
      box-shadow: var(--shadow-lg);
    }
    aside:not(.open) .scroll {
      display: none;
    }
    .scanbar {
      padding-bottom: calc(14px + env(safe-area-inset-bottom));
    }
    .scan-row .btn {
      height: 56px;
      font-size: 17px;
      border-radius: 14px;
    }
    .settings-toggle {
      display: inline-flex;
      width: 56px;
      padding: 0;
    }
    .sheet-scrim {
      display: block;
      position: fixed;
      inset: 0;
      z-index: 5;
      border: 0;
      background: rgb(10 12 18 / 40%);
      animation: fade 0.15s ease-out;
    }
    .preview {
      position: fixed;
      inset: 0;
      z-index: 8;
      width: auto;
      min-width: 0;
      border-left: 0;
      animation: fade 0.15s ease-out;
    }
    .preview-img {
      padding: 12px;
    }
    .toast {
      bottom: calc(96px + env(safe-area-inset-bottom));
    }
  }

  .empty {
    margin: auto;
    text-align: center;
    max-width: 360px;
    padding: 24px;
  }
  .empty h2 {
    margin: 22px 0 6px;
    font-size: 18px;
  }
  .empty p {
    margin: 0;
  }
  .art {
    position: relative;
    width: 120px;
    height: 120px;
    margin: 0 auto;
  }
  .sheet {
    position: absolute;
    width: 72px;
    height: 94px;
    left: 24px;
    top: 13px;
    border-radius: 6px;
    background: var(--surface);
    border: 1px solid var(--border);
    box-shadow: var(--shadow);
  }
  .s1 {
    transform: rotate(-9deg) translateX(-10px);
  }
  .s2 {
    transform: rotate(6deg) translateX(10px);
  }
  .s3 {
    background:
      linear-gradient(var(--border) 0 0) 12px 18px / 48px 4px no-repeat,
      linear-gradient(var(--border) 0 0) 12px 28px / 40px 4px no-repeat,
      linear-gradient(var(--border) 0 0) 12px 38px / 44px 4px no-repeat,
      linear-gradient(var(--accent-soft) 0 0) 12px 54px / 48px 22px no-repeat,
      var(--surface);
  }

  .scrim {
    position: fixed;
    inset: 0;
    display: grid;
    place-items: center;
    background: rgb(10 12 18 / 40%);
    animation: fade 0.15s ease-out;
    z-index: 10;
  }
  .dialog {
    width: min(400px, calc(100vw - 32px));
    padding: 26px 24px 18px;
    border-radius: 14px;
    background: var(--surface);
    border: 1px solid var(--border);
    box-shadow: var(--shadow-lg);
    text-align: center;
    animation: pop 0.18s ease-out;
  }
  .dialog-icon {
    display: inline-grid;
    place-items: center;
    width: 44px;
    height: 44px;
    border-radius: 12px;
    background: var(--accent-soft);
    color: var(--accent);
  }
  .dialog h2 {
    margin: 12px 0 6px;
    font-size: 17px;
  }
  .dialog p {
    margin: 0 0 18px;
  }
  .dialog-actions {
    display: grid;
    grid-template-columns: 1fr 1.4fr;
    gap: 10px;
    margin-bottom: 10px;
  }

  .toast {
    position: fixed;
    bottom: 20px;
    left: 50%;
    transform: translateX(-50%);
    display: flex;
    align-items: center;
    gap: 10px;
    max-width: min(560px, calc(100vw - 32px));
    padding: 10px 10px 10px 14px;
    border-radius: 10px;
    background: var(--text);
    color: var(--bg);
    box-shadow: var(--shadow-lg);
    animation: rise 0.2s ease-out;
    z-index: 20;
  }
  .toast.error {
    background: var(--danger);
    color: #fff;
  }
  .toast .link {
    border: 0;
    background: transparent;
    color: inherit;
    font-weight: 600;
    text-decoration: underline;
    padding: 0 4px;
  }
  .toast .close {
    display: grid;
    border: 0;
    background: transparent;
    color: inherit;
    opacity: 0.7;
    padding: 4px;
  }

  .spinner {
    width: 14px;
    height: 14px;
    border-radius: 50%;
    border: 2px solid var(--border);
    border-top-color: var(--accent);
    animation: spin 0.8s linear infinite;
  }
  .spinner.big {
    width: 28px;
    height: 28px;
    border-width: 3px;
  }

  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
  @keyframes fade {
    from {
      opacity: 0;
    }
  }
  @keyframes pop {
    from {
      opacity: 0;
      transform: scale(0.97);
    }
  }
  @keyframes rise {
    from {
      opacity: 0;
      transform: translate(-50%, 8px);
    }
  }
  @keyframes slide {
    from {
      opacity: 0;
      transform: translateX(12px);
    }
  }
</style>
