<script lang="ts">
  import "../app.css";
  import { onMount } from "svelte";
  import { isWeb } from "$lib/api";
  let { children } = $props();

  // Installable web app. Browsers only allow this on HTTPS or localhost.
  onMount(() => {
    if (isWeb && "serviceWorker" in navigator) navigator.serviceWorker.register("/sw.js").catch(() => {});
  });
</script>

{@render children()}
