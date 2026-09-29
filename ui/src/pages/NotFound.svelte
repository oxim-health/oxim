<script lang="ts">
  import { router } from '../lib/router.svelte';
</script>

<svelte:head><title>Page not found · OXIM</title></svelte:head>

<h1 tabindex="-1">Page not found</h1>
<p>
  There is no page at <span class="mono">{router.pathname}</span>. <a href="/">Go to the start page</a>.
</p>
