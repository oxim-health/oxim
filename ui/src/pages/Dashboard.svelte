<script lang="ts">
  import { onDestroy, onMount } from 'svelte';
  import ErrorNotice from '../lib/components/ErrorNotice.svelte';
  import StatusBadge from '../lib/components/StatusBadge.svelte';
  import { api, isBroken, type Channel, type SystemInfo } from '../lib/api';
  import { formatAge, formatClock, formatCount, formatDuration } from '../lib/format';
  import { LiveStats } from '../lib/live.svelte';
  import { session } from '../lib/session.svelte';
  import { summarize, totals } from '../lib/stats';

  const live = new LiveStats();
  let system = $state<SystemInfo | null>(null);
  let channels = $state<Channel[]>([]);
  let error = $state<unknown>(null);

  let rows = $derived(live.latest ? summarize(live.latest) : []);
  let sum = $derived(totals(rows));
  let names = $derived(new Map(channels.map((channel) => [channel.id, channel.name ?? null])));
  let oldest = $derived(
    new Map(
      channels.map((channel) => [
        channel.id,
        channel.destinations
          .map((destination) => destination.queue?.oldest_pending_at ?? null)
          .filter((value): value is string => value !== null)
          .sort()[0] ?? null,
      ]),
    ),
  );

  async function refresh() {
    try {
      const [info, list] = await Promise.all([
        session.can('view_system') ? api.system() : Promise.resolve(null),
        session.can('view_channels') ? api.channels() : Promise.resolve(null),
      ]);
      system = info;
      channels = list ? list.channels.filter((entry): entry is Channel => !isBroken(entry)) : [];
      error = null;
    } catch (failure) {
      error = failure;
    }
  }

  let timer: ReturnType<typeof setInterval> | undefined;
  onMount(() => {
    live.open();
    void refresh();
    timer = setInterval(() => void refresh(), 15_000);
  });
  onDestroy(() => {
    live.close();
    clearInterval(timer);
  });

  const TILES = [
    ['received', 'Received', 'All stored messages'],
    ['queued', 'Queued', 'Deliveries waiting or retrying'],
    ['sent', 'Sent', 'Deliveries completed'],
    ['filtered', 'Filtered', 'Messages stopped by filters'],
    ['errored', 'Errored', 'Failed messages and deliveries'],
  ] as const;
</script>

<svelte:head><title>Dashboard · OXIM</title></svelte:head>

<div class="page-header">
  <div>
    <h1 tabindex="-1">Dashboard</h1>
    <p>Message flow per channel, updated live.</p>
  </div>
  <div class="row" role="status" aria-live="polite">
    <StatusBadge status={live.state} />
    {#if live.updatedAt}
      <span class="muted small-text">Updated {formatClock(live.updatedAt)}</span>
    {/if}
  </div>
</div>

<ErrorNotice {error} />

<section aria-labelledby="totals-heading" class="tiles-section">
  <h2 id="totals-heading" class="visually-hidden">Totals</h2>
  <ul class="tiles">
    {#each TILES as [key, label, hint] (key)}
      <li class="tile panel">
        <span class="tile-label">{label}</span>
        <span class="tile-value">{formatCount(sum[key])}</span>
        <span class="tile-hint muted">
          {#if key === 'errored' && sum.errored > 0}
            <span class="badge bad">Needs attention</span>
          {:else if key === 'queued' && sum.retrying > 0}
            <span class="badge warn">{formatCount(sum.retrying)} retrying</span>
          {:else}
            {hint}
          {/if}
        </span>
      </li>
    {/each}
  </ul>
</section>

<section class="panel" aria-labelledby="channels-heading">
  <div class="spread">
    <h2 id="channels-heading">Channels</h2>
    {#if session.can('view_messages')}
      <a href="/messages?status=error" class="small-text">Show errored messages</a>
    {/if}
  </div>
  {#if !live.latest}
    <p class="muted" role="status">Waiting for the first update…</p>
  {:else if rows.length === 0}
    <p class="muted">No channel is deployed and no messages are stored yet.</p>
  {:else}
    <div class="table-wrap">
      <table>
        <caption class="visually-hidden">Message counts per channel</caption>
        <thead>
          <tr>
            <th scope="col">Channel</th>
            <th scope="col">State</th>
            <th scope="col" class="num">Received</th>
            <th scope="col" class="num">Filtered</th>
            <th scope="col" class="num">Queued</th>
            <th scope="col" class="num">Sent</th>
            <th scope="col" class="num">Errored</th>
            <th scope="col">Oldest waiting</th>
          </tr>
        </thead>
        <tbody>
          {#each rows as row (row.channel)}
            <tr>
              <th scope="row">
                {#if session.can('view_messages')}
                  <a href="/messages?channel={encodeURIComponent(row.channel)}">{row.channel}</a>
                {:else}
                  {row.channel}
                {/if}
                {#if names.get(row.channel)}
                  <div class="muted small-text">{names.get(row.channel)}</div>
                {/if}
              </th>
              <td><StatusBadge status={row.deployed ? 'deployed' : 'stopped'} /></td>
              <td class="num">{formatCount(row.received)}</td>
              <td class="num">{formatCount(row.filtered)}</td>
              <td class="num">
                {formatCount(row.queued)}
                {#if row.retrying > 0}<span class="badge warn">{formatCount(row.retrying)} retrying</span>{/if}
              </td>
              <td class="num">{formatCount(row.sent)}</td>
              <td class="num">
                {#if row.errored > 0}
                  <span class="badge bad">{formatCount(row.errored)} errored</span>
                {:else}
                  0
                {/if}
              </td>
              <td class="nowrap">{formatAge(oldest.get(row.channel))}</td>
            </tr>
          {/each}
        </tbody>
      </table>
    </div>
  {/if}
</section>

{#if system}
  <section class="panel system" aria-labelledby="system-heading">
    <h2 id="system-heading">System</h2>
    <dl class="facts">
      <dt>Version</dt>
      <dd>OXIM {system.version}</dd>
      <dt>Uptime</dt>
      <dd>{formatDuration(system.uptime_seconds)}</dd>
      <dt>Deployed channels</dt>
      <dd>{system.deployed_channels.length}</dd>
      <dt>Web server</dt>
      <dd>{system.tls ? 'HTTPS' : 'HTTP (no TLS)'}</dd>
    </dl>
    {#if session.can('view_system')}
      <p class="small-text"><a href="/system">System health details</a></p>
    {/if}
  </section>
{/if}

<style>
  .tiles-section {
    margin-bottom: 1rem;
  }

  .tiles {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(10rem, 1fr));
    gap: 0.75rem;
  }

  .tile {
    display: flex;
    flex-direction: column;
    gap: 0.2rem;
    padding: 0.85rem 1rem;
  }

  .tile-label {
    font-size: 0.85rem;
    font-weight: 600;
    color: var(--muted);
  }

  .tile-value {
    font-size: 1.9rem;
    font-weight: 650;
    font-variant-numeric: tabular-nums;
    line-height: 1.1;
  }

  .tile-hint {
    font-size: 0.8rem;
    min-height: 1.3rem;
  }

  .system {
    margin-top: 1rem;
  }

  td .badge {
    margin-left: 0.35rem;
  }

  th[scope='row'] {
    background: none;
    position: static;
    font-weight: 600;
  }
</style>
