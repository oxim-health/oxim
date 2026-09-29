<script lang="ts">
  // A collapsible tree of a JSON value, built from <details> elements so
  // it is keyboard- and screen-reader-accessible without extra code.
  import JsonTree from './JsonTree.svelte';

  let { value, name = null, depth = 0 }: { value: unknown; name?: string | null; depth?: number } =
    $props();

  let entries = $derived(
    Array.isArray(value)
      ? value.map((item, index) => [String(index), item] as [string, unknown])
      : value !== null && typeof value === 'object'
        ? Object.entries(value as Record<string, unknown>)
        : [],
  );
  let composite = $derived(value !== null && typeof value === 'object');
  let summary = $derived(Array.isArray(value) ? `[${entries.length}]` : `{${entries.length}}`);
</script>

{#if composite}
  <details open={depth < 3}>
    <summary>
      {#if name !== null}<span class="key">{name}</span>{/if}
      <span class="muted">{summary}</span>
    </summary>
    <ul>
      {#each entries as [key, item] (key)}
        <li><JsonTree value={item} name={key} depth={depth + 1} /></li>
      {/each}
    </ul>
  </details>
{:else}
  <span class="leaf">
    {#if name !== null}<span class="key">{name}:</span>{/if}
    <span class="value {value === null ? 'null' : typeof value}">{JSON.stringify(value)}</span>
  </span>
{/if}

<style>
  details,
  .leaf {
    font-family: var(--mono);
    font-size: 0.85rem;
  }

  summary {
    cursor: pointer;
  }

  ul {
    list-style: none;
    margin: 0;
    padding-left: 1.1rem;
    border-left: 1px dotted var(--border-strong);
  }

  .key {
    color: var(--text);
    font-weight: 600;
    margin-right: 0.35rem;
  }

  .value.string {
    color: var(--ok);
  }

  .value.number {
    color: var(--info);
  }

  .value.boolean,
  .value.null {
    color: var(--warn);
  }
</style>
