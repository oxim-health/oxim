<script lang="ts">
  import { onMount } from 'svelte';
  import ErrorNotice from '../lib/components/ErrorNotice.svelte';
  import Time from '../lib/components/Time.svelte';
  import { api, type SystemInfo } from '../lib/api';
  import { formatDuration } from '../lib/format';

  let system = $state<SystemInfo | null>(null);
  let error = $state<unknown>(null);

  async function load() {
    try {
      system = await api.system();
      error = null;
    } catch (failure) {
      error = failure;
    }
  }

  onMount(load);

  const KIND_LABELS: Record<string, string> = {
    source: 'Source connectors',
    destination: 'Destination connectors',
    filter: 'Filters',
    transformer: 'Transformers',
    encoder: 'Encoders',
  };
</script>

<svelte:head><title>System health · OXIM</title></svelte:head>

<div class="page-header">
  <div>
    <h1 tabindex="-1">System health</h1>
    <p>The running OXIM instance and the components channels can use.</p>
  </div>
  <button type="button" onclick={load}>Refresh</button>
</div>

<ErrorNotice {error} />

{#if system}
  <div class="grid">
    <section class="panel" aria-labelledby="instance-heading">
      <h2 id="instance-heading">Instance</h2>
      <dl class="facts">
        <dt>Version</dt>
        <dd>OXIM {system.version}</dd>
        <dt>Started</dt>
        <dd><Time value={system.started_at} /></dd>
        <dt>Uptime</dt>
        <dd>{formatDuration(system.uptime_seconds)}</dd>
        <dt>Server time</dt>
        <dd><Time value={system.now} /></dd>
        <dt>Web server</dt>
        <dd>
          {#if system.tls}
            HTTPS
          {:else}
            <span class="badge warn">HTTP without TLS</span> Suitable for local access only.
          {/if}
        </dd>
        <dt>Session timeout</dt>
        <dd>
          {formatDuration(system.session_idle_seconds)} idle, {formatDuration(system.session_max_seconds)} at most
        </dd>
        <dt>Metrics</dt>
        <dd>Prometheus format at <span class="mono">/metrics</span> (needs a token with the viewer role or higher)</dd>
      </dl>
    </section>

    <section class="panel" aria-labelledby="deployed-heading">
      <h2 id="deployed-heading">Deployed channels ({system.deployed_channels.length})</h2>
      {#if system.deployed_channels.length === 0}
        <p class="muted">No channel is deployed.</p>
      {:else}
        <ul class="columns">
          {#each system.deployed_channels as channel (channel)}
            <li class="mono">{channel}</li>
          {/each}
        </ul>
      {/if}
    </section>
  </div>

  <section class="panel components" aria-labelledby="components-heading">
    <h2 id="components-heading">Installed components</h2>
    <div class="kinds">
      {#each Object.entries(system.component_types) as [kind, names] (kind)}
        <div>
          <h3>{KIND_LABELS[kind] ?? kind} ({names.length})</h3>
          <ul>
            {#each names as name (name)}<li class="mono">{name}</li>{/each}
          </ul>
        </div>
      {/each}
    </div>
  </section>
{:else if !error}
  <p class="muted" role="status">Loading…</p>
{/if}

<style>
  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(22rem, 1fr));
    gap: 1rem;
    align-items: start;
  }

  .components {
    margin-top: 1rem;
  }

  .kinds {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(14rem, 1fr));
    gap: 1rem;
  }

  ul {
    margin: 0;
    padding-left: 1.1rem;
  }

  .columns {
    columns: 2;
  }
</style>
