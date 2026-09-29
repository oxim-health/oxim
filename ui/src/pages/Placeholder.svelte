<script lang="ts">
  let { title, summary }: { title: string; summary: string } = $props();
</script>

<svelte:head><title>{title} · OXIM</title></svelte:head>

<div class="page-header">
  <div>
    <h1 tabindex="-1">{title}</h1>
    <p>{summary}</p>
  </div>
</div>

<div class="panel">
  <p><strong>Not available in this build.</strong></p>
  <p class="muted">
    The server of this OXIM version has no API for this area yet. The page appears here once the
    API provides it.
  </p>
</div>
