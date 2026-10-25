<script lang="ts" module>
  export type Pick = { toggle: boolean; range: boolean };
</script>

<script lang="ts">
  import type { PageView } from "$lib/api";
  import Icon from "./Icon.svelte";

  let {
    pages,
    selected,
    picked,
    onselect,
    onrotate,
    ondelete,
    onreorder,
  }: {
    pages: PageView[];
    /** The page shown in the preview. */
    selected: number | null;
    /** Pages in the multi-selection. */
    picked: number[];
    onselect: (id: number, how: Pick) => void;
    onrotate: (id: number, clockwise: boolean) => void;
    ondelete: (id: number) => void;
    onreorder: (order: number[]) => void;
  } = $props();

  // Reordering uses pointer events, so mouse, touch and pen all work: the
  // browser's own drag and drop doesn't fire for touch on phones. Mouse: press
  // and move. Touch: long-press, then move; a quick swipe still scrolls.
  const LONG_PRESS_MS = 300;
  /** How far a finger may wander during the long press before it counts as a scroll. */
  const TOUCH_SLOP = 8;
  const MOUSE_SLOP = 5;
  /** Distance from the top or bottom edge where the list scrolls while dragging. */
  const EDGE = 90;

  type Drag = {
    id: number;
    pointerId: number;
    touch: boolean;
    startX: number;
    startY: number;
    x: number;
    y: number;
    active: boolean;
    scroller: HTMLElement;
    startScroll: number;
    /** The list's scroll limit before the lifted page moved, which would extend it. */
    maxScroll: number;
    timer?: ReturnType<typeof setTimeout>;
    frame?: number;
  };

  let grid: HTMLOListElement;
  let drag: Drag | null = null;
  let dragId: number | null = $state(null);
  /** Slot the page would land in: 0 = before the first page, pages.length = after the last. */
  let overIndex: number | null = $state(null);
  let offset = $state({ x: 0, y: 0 });
  let suppressClick = false;

  const fromIndex = $derived(pages.findIndex((p) => p.id === dragId));
  // Dropping right before or after itself changes nothing, so show no marker.
  const marker = $derived(overIndex === null || overIndex === fromIndex || overIndex === fromIndex + 1 ? null : overIndex);

  function scrollParent(el: HTMLElement): HTMLElement {
    for (let p = el.parentElement; p; p = p.parentElement) {
      const o = getComputedStyle(p).overflowY;
      if ((o === "auto" || o === "scroll") && p.scrollHeight > p.clientHeight) return p;
    }
    return (document.scrollingElement as HTMLElement) ?? document.documentElement;
  }

  function onpointerdown(e: PointerEvent, id: number) {
    if (drag || e.button !== 0 || (e.target as HTMLElement).closest(".check, .mini")) return;
    const scroller = scrollParent(e.currentTarget as HTMLElement);
    drag = {
      id,
      pointerId: e.pointerId,
      touch: e.pointerType !== "mouse",
      startX: e.clientX,
      startY: e.clientY,
      x: e.clientX,
      y: e.clientY,
      active: false,
      scroller,
      startScroll: scroller.scrollTop,
      maxScroll: scroller.scrollHeight - scroller.clientHeight,
    };
    if (drag.touch) drag.timer = setTimeout(activate, LONG_PRESS_MS);
  }

  function activate() {
    if (!drag || drag.active) return;
    drag.active = true;
    drag.maxScroll = drag.scroller.scrollHeight - drag.scroller.clientHeight;
    dragId = drag.id;
    navigator.vibrate?.(10);
    track();
    drag.frame = requestAnimationFrame(autoscroll);
  }

  function onpointermove(e: PointerEvent) {
    if (!drag || e.pointerId !== drag.pointerId) return;
    drag.x = e.clientX;
    drag.y = e.clientY;
    if (!drag.active) {
      const moved = Math.hypot(e.clientX - drag.startX, e.clientY - drag.startY);
      if (drag.touch && moved > TOUCH_SLOP) stop(); // a scroll, not a long press
      else if (!drag.touch && moved > MOUSE_SLOP) activate();
      return;
    }
    track();
  }

  /** Move the lifted page with the pointer and work out where it would land. */
  function track() {
    if (!drag) return;
    offset = {
      x: drag.x - drag.startX,
      y: drag.y - drag.startY + drag.scroller.scrollTop - drag.startScroll,
    };
    // Look through overlays (the phone's bottom bar covers the lower edge, where
    // auto-scroll happens); the lifted page itself ignores the pointer.
    const under = document
      .elementsFromPoint(drag.x, drag.y)
      .map((el) => el.closest<HTMLElement>("li.card"))
      .find((el) => el && grid.contains(el));
    if (under) {
      const i = Number(under.dataset.index);
      const r = under.getBoundingClientRect();
      overIndex = drag.x > r.left + r.width / 2 ? i + 1 : i;
      return;
    }
    // Past the last page or before the first; between pages, keep the last slot.
    const cards = grid.querySelectorAll<HTMLElement>("li.card");
    const first = cards[0]?.getBoundingClientRect();
    const last = cards[cards.length - 1]?.getBoundingClientRect();
    if (last && drag.y > last.bottom) overIndex = pages.length;
    else if (first && drag.y < first.top) overIndex = 0;
  }

  function autoscroll() {
    if (!drag?.active) return;
    const s = drag.scroller;
    const box = s === document.scrollingElement ? { top: 0, bottom: innerHeight } : s.getBoundingClientRect();
    const top = Math.max(box.top, 0);
    const bottom = Math.min(box.bottom, innerHeight);
    const depth = drag.y < top + EDGE ? drag.y - (top + EDGE) : drag.y > bottom - EDGE ? drag.y - (bottom - EDGE) : 0;
    if (depth) {
      const before = s.scrollTop;
      const step = (Math.max(-EDGE, Math.min(EDGE, depth)) / EDGE) * 16;
      s.scrollTop = Math.max(0, Math.min(drag.maxScroll, before + step));
      if (s.scrollTop !== before) track();
    }
    drag.frame = requestAnimationFrame(autoscroll);
  }

  function onpointerup(e: PointerEvent) {
    if (!drag || e.pointerId !== drag.pointerId) return;
    const { id, active } = drag;
    const slot = overIndex;
    stop();
    if (!active) return; // a tap or click: the buttons handle it
    // The click that follows the release isn't a "select this page".
    suppressClick = true;
    setTimeout(() => (suppressClick = false), 0);
    const from = pages.findIndex((p) => p.id === id);
    if (slot === null || from === -1) return;
    const to = slot > from ? slot - 1 : slot;
    if (to === from) return;
    const order = pages.map((p) => p.id).filter((x) => x !== id);
    order.splice(to, 0, id);
    onreorder(order);
  }

  function stop() {
    if (drag) {
      clearTimeout(drag.timer);
      if (drag.frame) cancelAnimationFrame(drag.frame);
    }
    drag = null;
    dragId = overIndex = null;
    offset = { x: 0, y: 0 };
  }

  // While a page is lifted, a moving finger drags it instead of scrolling. This
  // has to be a non-passive listener to be allowed to cancel the scroll.
  $effect(() => {
    const block = (e: TouchEvent) => {
      if (drag?.active) e.preventDefault();
    };
    window.addEventListener("touchmove", block, { passive: false });
    return () => window.removeEventListener("touchmove", block);
  });
</script>

<svelte:window
  {onpointermove}
  {onpointerup}
  onpointercancel={stop}
  onkeydown={(e) => {
    if (e.key === "Escape" && dragId !== null) stop();
  }}
/>

<ol class="grid" class:picking={picked.length > 1} class:reordering={dragId !== null} aria-label="Scanned pages" bind:this={grid}>
  {#each pages as page, i (page.id)}
    {@const isPicked = picked.includes(page.id)}
    <li
      class="card"
      class:selected={selected === page.id}
      class:picked={isPicked}
      class:dragging={dragId === page.id}
      class:drop-before={marker === i}
      class:drop-after={marker === pages.length && i === pages.length - 1}
      data-index={i}
      style:--dx={dragId === page.id ? `${offset.x}px` : null}
      style:--dy={dragId === page.id ? `${offset.y}px` : null}
      onpointerdown={(e) => onpointerdown(e, page.id)}
      oncontextmenu={(e) => e.preventDefault()}
    >
      <button
        class="thumb"
        onclick={(e) => {
          if (!suppressClick) onselect(page.id, { toggle: e.metaKey || e.ctrlKey, range: e.shiftKey });
        }}
        aria-label={`Page ${i + 1}`}
      >
        <img src={page.thumb} alt="" style:aspect-ratio={`${page.width} / ${page.height}`} draggable="false" />
      </button>
      <button
        class="check"
        role="checkbox"
        aria-checked={isPicked}
        aria-label={`Select page ${i + 1}`}
        onclick={(e) => onselect(page.id, { toggle: !e.shiftKey, range: e.shiftKey })}
      >
        {#if isPicked}<Icon name="check" size={13} stroke={3} />{/if}
      </button>
      <div class="meta">
        <span class="num">{i + 1}</span>
        <span class="actions">
          <button class="mini" title="Rotate left" onclick={() => onrotate(page.id, false)}>
            <Icon name="rotate-ccw" size={14} />
          </button>
          <button class="mini" title="Rotate right" onclick={() => onrotate(page.id, true)}>
            <Icon name="rotate-cw" size={14} />
          </button>
          <button class="mini del" title="Delete page" onclick={() => ondelete(page.id)}>
            <Icon name="trash" size={14} />
          </button>
        </span>
      </div>
    </li>
  {/each}
</ol>

<style>
  .grid {
    list-style: none;
    margin: 0;
    padding: 20px;
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(150px, 1fr));
    gap: 18px;
    align-items: end;
  }
  .card {
    position: relative;
    display: flex;
    flex-direction: column;
    gap: 6px;
    animation: pop 0.22s ease-out;
  }
  .card {
    /* No "save image" menu on a long-press (iOS). */
    -webkit-touch-callout: none;
  }
  /* The lifted page follows the pointer; others stay put with a marker. */
  .card.dragging {
    z-index: 5;
    pointer-events: none;
    transform: translate(var(--dx, 0), var(--dy, 0)) scale(1.05);
    animation: none;
  }
  .card.dragging .thumb {
    box-shadow: var(--shadow-lg);
    border-color: var(--accent);
  }
  .reordering {
    cursor: grabbing;
  }
  .card.drop-before::before,
  .card.drop-after::after {
    content: "";
    position: absolute;
    top: 0;
    bottom: 28px;
    width: 3px;
    border-radius: 2px;
    background: var(--accent);
  }
  .card.drop-before::before {
    left: -11px;
  }
  .card.drop-after::after {
    right: -11px;
  }
  .thumb {
    padding: 0;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--surface);
    box-shadow: var(--shadow);
    overflow: hidden;
    transition: transform 0.12s, box-shadow 0.12s, border-color 0.12s;
  }
  .thumb:hover {
    transform: translateY(-2px);
  }
  .selected .thumb,
  .picked .thumb {
    border-color: var(--accent);
    box-shadow: 0 0 0 3px var(--accent-soft), var(--shadow);
  }
  .check {
    position: absolute;
    top: 8px;
    left: 8px;
    display: grid;
    place-items: center;
    width: 22px;
    height: 22px;
    padding: 0;
    border-radius: 6px;
    border: 1.5px solid var(--border);
    background: color-mix(in srgb, var(--surface) 92%, transparent);
    color: var(--on-accent);
    box-shadow: var(--shadow-sm);
    opacity: 0;
    transition: opacity 0.12s, background 0.12s;
  }
  .card:hover .check,
  .check:focus-visible,
  .picking .check,
  .picked .check {
    opacity: 1;
  }
  .picked .check {
    background: var(--accent);
    border-color: var(--accent);
  }
  img {
    display: block;
    width: 100%;
    height: auto;
    object-fit: contain;
    background: #fff;
  }
  .meta {
    display: flex;
    align-items: center;
    justify-content: space-between;
    height: 22px;
  }
  .num {
    font-size: 12px;
    font-weight: 600;
    color: var(--text-2);
    padding-left: 2px;
  }
  .actions {
    display: flex;
    gap: 2px;
    opacity: 0;
    transition: opacity 0.12s;
  }
  .card:hover .actions,
  .card:focus-within .actions,
  .selected .actions {
    opacity: 1;
  }
  .mini {
    display: grid;
    place-items: center;
    width: 24px;
    height: 22px;
    border: 0;
    border-radius: 5px;
    background: transparent;
    color: var(--text-2);
  }
  .mini:hover {
    background: var(--surface-2);
    color: var(--text);
  }
  .mini.del:hover {
    background: var(--danger-soft);
    color: var(--danger);
  }
  /* Phones: two columns; no hover, so the page actions always show. */
  @media (max-width: 700px) {
    .grid {
      grid-template-columns: repeat(2, 1fr);
      padding: 12px;
      gap: 14px;
    }
    .actions {
      opacity: 1;
    }
  }
  @keyframes pop {
    from {
      opacity: 0;
      transform: scale(0.96) translateY(6px);
    }
  }
</style>
