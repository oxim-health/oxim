<script lang="ts">
  let { role }: { role: string } = $props();
</script>

<svelte:head><title>Not permitted · OXIM</title></svelte:head>

<h1 tabindex="-1">Not permitted</h1>
<p>Your role ({role}) does not allow this page. Ask an administrator if you need access.</p>
