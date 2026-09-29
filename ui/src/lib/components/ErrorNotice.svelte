<script lang="ts">
  import { ApiError } from '../api';

  let { error, title = 'Something went wrong' }: { error: unknown; title?: string } = $props();

  let message = $derived(
    error instanceof ApiError || error instanceof Error ? error.message : String(error ?? ''),
  );
  let forbidden = $derived(error instanceof ApiError && error.status === 403);
</script>

{#if error}
  <div class="notice error" role="alert">
    <p><strong>{forbidden ? 'Not permitted' : title}:</strong> {message}</p>
  </div>
{/if}
