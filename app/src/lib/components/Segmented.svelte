<script lang="ts" generics="T extends string | boolean">
  let {
    value = $bindable(),
    options,
    label,
    disabled = false,
  }: {
    value: T;
    options: { value: T; label: string; disabled?: boolean }[];
    label: string;
    disabled?: boolean;
  } = $props();
</script>

<div class="seg" role="radiogroup" aria-label={label}>
  {#each options as o (String(o.value))}
    <button
      type="button"
      role="radio"
      aria-checked={value === o.value}
      class:on={value === o.value}
      disabled={disabled || o.disabled}
      onclick={() => (value = o.value)}
    >
      {o.label}
    </button>
  {/each}
</div>

<style>
  .seg {
    display: flex;
    padding: 3px;
    gap: 2px;
    border-radius: var(--radius-sm);
    background: var(--surface-2);
    border: 1px solid var(--border);
  }
  button {
    flex: 1;
    height: 28px;
    border: 0;
    border-radius: 5px;
    background: transparent;
    color: var(--text-2);
    font-weight: 500;
    transition: background 0.12s, color 0.12s;
  }
  button:hover:not(:disabled):not(.on) {
    color: var(--text);
  }
  button.on {
    background: var(--surface);
    color: var(--text);
    box-shadow: var(--shadow-sm);
  }
  button:disabled {
    opacity: 0.4;
    cursor: default;
  }
</style>
