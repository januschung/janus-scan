<script lang="ts">
  let {
    checked = $bindable(),
    label,
    hint = "",
    disabled = false,
  }: { checked: boolean; label: string; hint?: string; disabled?: boolean } = $props();
</script>

<label class="row" class:disabled>
  <span class="text">
    <span>{label}</span>
    {#if hint}<span class="hint">{hint}</span>{/if}
  </span>
  <input type="checkbox" role="switch" bind:checked {disabled} />
</label>

<style>
  .row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    padding: 6px 0;
    cursor: pointer;
  }
  .row.disabled {
    opacity: 0.5;
    cursor: default;
  }
  .text {
    display: flex;
    flex-direction: column;
  }
  .hint {
    font-size: 12px;
    color: var(--text-3);
  }
  input {
    appearance: none;
    flex: none;
    width: 34px;
    height: 20px;
    border-radius: 999px;
    background: var(--border);
    position: relative;
    margin: 0;
    cursor: inherit;
    transition: background 0.15s;
  }
  input::after {
    content: "";
    position: absolute;
    top: 2px;
    left: 2px;
    width: 16px;
    height: 16px;
    border-radius: 50%;
    background: #fff;
    box-shadow: 0 1px 2px rgb(0 0 0 / 25%);
    transition: transform 0.15s;
  }
  input:checked {
    background: var(--accent);
  }
  input:checked::after {
    transform: translateX(14px);
  }
</style>
