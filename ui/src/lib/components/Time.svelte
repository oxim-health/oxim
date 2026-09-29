<script lang="ts">
  import { formatTime } from '../format';

  let { value }: { value: string | null | undefined } = $props();
</script>

{#if value}
  <time datetime={value} title={value}>{formatTime(value)}</time>
{:else}
  <span class="muted" aria-label="none">—</span>
{/if}
