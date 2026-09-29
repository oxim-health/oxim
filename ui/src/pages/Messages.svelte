<script lang="ts">
  import { onMount } from 'svelte';
  import ErrorNotice from '../lib/components/ErrorNotice.svelte';
  import StatusBadge from '../lib/components/StatusBadge.svelte';
  import Time from '../lib/components/Time.svelte';
  import { api, isBroken, type MessageRecord } from '../lib/api';
  import { withQuery } from '../lib/api/client';
  import { isoToLocalInput, localInputToIso } from '../lib/format';
  import { router } from '../lib/router.svelte';
  import { session } from '../lib/session.svelte';

  const STATUSES = ['received', 'filtered', 'transformed', 'completed', 'error'];
  const DESTINATION_STATUSES = ['queued', 'sending', 'sent', 'filtered', 'retrying', 'failed'];
  const LIMITS = ['25', '50', '100', '200'];

  let channelIds = $state<string[]>([]);
  let messages = $state<MessageRecord[] | null>(null);
  let next = $state<string | null>(null);
  let error = $state<unknown>(null);

  // The form mirrors the URL, so searches can be bookmarked and shared.
  let form = $state({ channel: '', status: '', destination_status: '', from: '', until: '', limit: '50' });
  let query = $derived(router.pathname === '/messages' ? router.query : new URLSearchParams());
  let before = $derived(query.get('before'));

  $effect(() => {
    form = {
      channel: query.get('channel') ?? '',
      status: query.get('status') ?? '',
      destination_status: query.get('destination_status') ?? '',
      from: isoToLocalInput(query.get('from')),
      until: isoToLocalInput(query.get('until')),
      limit: query.get('limit') ?? '50',
    };
    void load(query);
  });

  onMount(async () => {
    if (!session.can('view_channels')) return;
    try {
      const list = await api.channels();
      channelIds = list.channels.flatMap((entry) => (isBroken(entry) ? [] : [entry.id]));
    } catch {
      // The channel filter falls back to a text field.
    }
  });

  let loadCount = 0;
  async function load(params: URLSearchParams) {
    const ticket = ++loadCount;
    messages = null;
    try {
      const result = await api.messages({
        channel: params.get('channel'),
        status: params.get('status'),
        destination_status: params.get('destination_status'),
        from: params.get('from'),
        until: params.get('until'),
        before: params.get('before'),
        limit: params.get('limit') ?? '50',
      });
      if (ticket !== loadCount) return;
      messages = result.messages;
      next = result.next_before;
      error = null;
    } catch (failure) {
      if (ticket !== loadCount) return;
      error = failure;
      messages = [];
    }
  }

  function search(extra: Record<string, string | null> = {}) {
    router.navigate(
      withQuery('/messages', {
        channel: form.channel,
        status: form.status,
        destination_status: form.destination_status,
        from: localInputToIso(form.from),
        until: localInputToIso(form.until),
        limit: form.limit === '50' ? undefined : form.limit,
        ...extra,
      }),
    );
  }

  function submit(event: SubmitEvent) {
    event.preventDefault();
    search();
  }

  function reset() {
    form = { channel: '', status: '', destination_status: '', from: '', until: '', limit: '50' };
    router.navigate('/messages');
  }
</script>

<svelte:head><title>Messages · OXIM</title></svelte:head>

<div class="page-header">
  <div>
    <h1 tabindex="-1">Messages</h1>
    <p>Stored messages, newest first. Patient data is masked unless your role allows it.</p>
  </div>
</div>

<form class="panel filters" onsubmit={submit} aria-label="Message filters">
  <div class="form-grid">
    <div class="field">
      <label for="filter-channel">Channel</label>
      {#if channelIds.length > 0}
        <select id="filter-channel" bind:value={form.channel}>
          <option value="">All channels</option>
          {#each channelIds as channel (channel)}<option value={channel}>{channel}</option>{/each}
        </select>
      {:else}
        <input id="filter-channel" bind:value={form.channel} placeholder="All channels" />
      {/if}
    </div>
    <div class="field">
      <label for="filter-status">Message status</label>
      <select id="filter-status" bind:value={form.status}>
        <option value="">Any</option>
        {#each STATUSES as status (status)}<option value={status}>{status}</option>{/each}
      </select>
    </div>
    <div class="field">
      <label for="filter-destination">Delivery status</label>
      <select id="filter-destination" bind:value={form.destination_status}>
        <option value="">Any</option>
        {#each DESTINATION_STATUSES as status (status)}<option value={status}>{status}</option>{/each}
      </select>
    </div>
    <div class="field">
      <label for="filter-from">Received from</label>
      <input id="filter-from" type="datetime-local" bind:value={form.from} />
    </div>
    <div class="field">
      <label for="filter-until">Received until</label>
      <input id="filter-until" type="datetime-local" bind:value={form.until} />
    </div>
    <div class="field">
      <label for="filter-limit">Per page</label>
      <select id="filter-limit" bind:value={form.limit}>
        {#each LIMITS as limit (limit)}<option value={limit}>{limit}</option>{/each}
      </select>
    </div>
  </div>
  <div class="row buttons">
    <button type="submit" class="primary">Search</button>
    <button type="button" onclick={reset}>Clear filters</button>
  </div>
</form>

<ErrorNotice {error} />

<section aria-labelledby="results-heading" aria-busy={messages === null}>
  <div class="spread results-header">
    <h2 id="results-heading">Results</h2>
    <div class="row">
      {#if before}
        <button type="button" onclick={() => search({ before: null })}>Newest</button>
      {/if}
      <button type="button" disabled={!next} onclick={() => next && search({ before: next })}>Older</button>
    </div>
  </div>
  {#if messages === null}
    <p class="muted" role="status">Loading messages…</p>
  {:else if messages.length === 0}
    <p class="muted" role="status">No messages match.</p>
  {:else}
    <div class="table-wrap">
      <table>
        <caption class="visually-hidden">Messages{before ? ' older than the previous page' : ''}</caption>
        <thead>
          <tr>
            <th scope="col">Received</th>
            <th scope="col">Message</th>
            <th scope="col">Channel</th>
            <th scope="col">Status</th>
            <th scope="col">Deliveries</th>
            <th scope="col">From</th>
            <th scope="col">Error</th>
          </tr>
        </thead>
        <tbody>
          {#each messages as message (message.id)}
            <tr>
              <td class="nowrap"><Time value={message.received_at} /></td>
              <td><a class="mono" href="/messages/{message.id}">{message.id}</a></td>
              <td>{message.channel}</td>
              <td><StatusBadge status={message.status} /></td>
              <td>
                {#each message.destinations as destination (destination.destination)}
                  <div class="nowrap">
                    <span class="mono small-text">{destination.destination}</span>
                    <StatusBadge status={destination.status} />
                  </div>
                {/each}
              </td>
              <td class="small-text">{message.peer ?? message.device ?? '—'}</td>
              <td class="small-text error-cell">{message.error ?? ''}</td>
            </tr>
          {/each}
        </tbody>
      </table>
    </div>
  {/if}
</section>

<style>
  .filters {
    margin-bottom: 1rem;
  }

  .buttons {
    margin-top: 0.75rem;
  }

  .results-header {
    margin-bottom: 0.5rem;
  }

  .results-header h2 {
    margin: 0;
  }

  .error-cell {
    max-width: 22rem;
    overflow-wrap: anywhere;
  }
</style>
