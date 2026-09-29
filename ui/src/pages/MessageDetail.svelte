<script lang="ts">
  import { onMount } from 'svelte';
  import ContentPane from '../lib/components/ContentPane.svelte';
  import Dialog from '../lib/components/Dialog.svelte';
  import ErrorNotice from '../lib/components/ErrorNotice.svelte';
  import StatusBadge from '../lib/components/StatusBadge.svelte';
  import Time from '../lib/components/Time.svelte';
  import { api, type MessageDetail } from '../lib/api';
  import { router } from '../lib/router.svelte';
  import { session } from '../lib/session.svelte';

  let { id }: { id: string } = $props();

  const REASON_MIN = 8;

  let message = $state<MessageDetail | null>(null);
  let error = $state<unknown>(null);
  let notice = $state<string | null>(null);
  let busy = $state(false);
  let left = $state(0);
  let right = $state(0);
  let reprocessOpen = $state(false);
  let eraseOpen = $state(false);
  let eraseReason = $state('');
  let eraseError = $state<string | null>(null);

  // Default comparison: the raw message against what was sent (or the
  // normalized or transformed form when nothing was sent).
  function defaults(detail: MessageDetail) {
    const contents = detail.contents;
    const raw = contents.findIndex((item) => item.stage === 'raw');
    const preferred = ['encoded', 'reply', 'transformed', 'normalized', 'response'];
    let other = -1;
    for (const stage of preferred) {
      other = contents.findIndex((item) => item.stage === stage);
      if (other >= 0) break;
    }
    left = raw >= 0 ? raw : 0;
    right = other >= 0 ? other : Math.min(1, contents.length - 1);
  }

  async function load(resetPanes = false) {
    try {
      const detail = await api.message(id);
      message = detail;
      if (resetPanes) defaults(detail);
      error = null;
    } catch (failure) {
      error = failure;
    }
  }

  onMount(() => load(true));

  async function reprocess(event: SubmitEvent) {
    event.preventDefault();
    reprocessOpen = false;
    busy = true;
    try {
      const result = await api.reprocess(id);
      notice = result.scheduled
        ? 'The message is being processed again.'
        : 'The message will be processed again when its channel is deployed.';
      await load(true);
    } catch (failure) {
      error = failure;
    } finally {
      busy = false;
    }
  }

  async function requeue(destination: string) {
    busy = true;
    try {
      await api.requeue(id, destination);
      notice = `The delivery to ${destination} is queued for an immediate attempt.`;
      await load();
    } catch (failure) {
      error = failure;
    } finally {
      busy = false;
    }
  }

  async function erase(event: SubmitEvent) {
    event.preventDefault();
    if (eraseReason.trim().length < REASON_MIN) {
      eraseError = `Give a reason of at least ${REASON_MIN} characters.`;
      return;
    }
    try {
      await api.erase(id, eraseReason.trim());
      eraseOpen = false;
      router.navigate('/messages');
    } catch (failure) {
      eraseError = failure instanceof Error ? failure.message : 'The message was not erased.';
    }
  }
</script>

<svelte:head><title>Message {id} · OXIM</title></svelte:head>

<div class="page-header">
  <div>
    <h1 tabindex="-1">Message <span class="mono id">{id}</span></h1>
    <p><a href="/messages">Back to messages</a></p>
  </div>
  <div class="row">
    {#if session.can('repair_messages')}
      <button type="button" onclick={() => (reprocessOpen = true)} disabled={busy || !message}>Reprocess</button>
    {/if}
    {#if session.can('erase_messages')}
      <button type="button" class="danger" onclick={() => (eraseOpen = true)} disabled={busy || !message}>Erase…</button>
    {/if}
  </div>
</div>

<ErrorNotice {error} />
{#if notice}<div class="notice success" role="status"><p>{notice}</p></div>{/if}

{#if message}
  <div class="summary">
    <section class="panel" aria-labelledby="record-heading">
      <h2 id="record-heading">Record</h2>
      <dl class="facts">
        <dt>Status</dt>
        <dd><StatusBadge status={message.status} /></dd>
        <dt>Channel</dt>
        <dd>
          {#if session.can('view_channels')}<a href="/channels/{encodeURIComponent(message.channel)}">{message.channel}</a>{:else}{message.channel}{/if}
          <span class="muted">via {message.connector}</span>
        </dd>
        <dt>Received</dt>
        <dd><Time value={message.received_at} /></dd>
        <dt>Data type</dt>
        <dd class="mono">{message.data_type}</dd>
        {#if message.peer}<dt>From</dt><dd class="mono">{message.peer}</dd>{/if}
        {#if message.device}<dt>Device</dt><dd>{message.device}</dd>{/if}
        {#if message.correlation_id}<dt>Correlation</dt><dd class="mono">{message.correlation_id}</dd>{/if}
        {#each Object.entries(message.metadata) as [key, value] (key)}
          <dt>{key}</dt>
          <dd class="mono">{value}</dd>
        {/each}
        {#if message.error}
          <dt>Error</dt>
          <dd class="error-text">{message.error}</dd>
        {/if}
      </dl>
    </section>

    <section class="panel" aria-labelledby="deliveries-heading">
      <h2 id="deliveries-heading">Deliveries</h2>
      {#if message.destinations.length === 0}
        <p class="muted">No deliveries: the message was stored only{message.status === 'filtered' ? ' (filtered)' : ''}.</p>
      {:else}
        <div class="table-wrap">
          <table>
            <thead>
              <tr>
                <th scope="col">Destination</th>
                <th scope="col">Status</th>
                <th scope="col" class="num">Attempts</th>
                <th scope="col">Next attempt</th>
                <th scope="col">Last error</th>
                {#if session.can('repair_messages')}<th scope="col"><span class="visually-hidden">Actions</span></th>{/if}
              </tr>
            </thead>
            <tbody>
              {#each message.destinations as delivery (delivery.destination)}
                <tr>
                  <td class="mono">{delivery.destination}</td>
                  <td><StatusBadge status={delivery.status} /></td>
                  <td class="num">{delivery.attempts}</td>
                  <td class="nowrap"><Time value={delivery.next_attempt_at} /></td>
                  <td class="small-text error-cell">{delivery.last_error ?? ''}</td>
                  {#if session.can('repair_messages')}
                    <td>
                      {#if delivery.status === 'failed' || delivery.status === 'retrying'}
                        <button
                          type="button"
                          class="small"
                          disabled={busy}
                          onclick={() => requeue(delivery.destination)}
                          aria-label="Retry delivery to {delivery.destination} now">Retry now</button
                        >
                      {/if}
                    </td>
                  {/if}
                </tr>
              {/each}
            </tbody>
          </table>
        </div>
      {/if}
    </section>
  </div>

  <section aria-labelledby="contents-heading" class="contents">
    <h2 id="contents-heading">Contents</h2>
    {#if message.contents.length === 0}
      <p class="muted">No content is stored for this message (it may have been pruned by retention).</p>
    {:else}
      <p class="muted small-text">Each content view is recorded in the audit log.</p>
      <div class="compare">
        <ContentPane messageId={id} contents={message.contents} bind:selected={left} label="Left view" />
        {#if message.contents.length > 1}
          <ContentPane messageId={id} contents={message.contents} bind:selected={right} label="Right view" />
        {/if}
      </div>
    {/if}
  </section>
{:else if !error}
  <p class="muted" role="status">Loading message…</p>
{/if}

<Dialog
  bind:open={reprocessOpen}
  title="Reprocess this message?"
  description="Everything derived from the message (normalized and transformed forms, queued deliveries) is discarded and the message runs through its channel again. Destinations may receive it a second time."
>
  <form class="row" onsubmit={reprocess}>
    <button type="submit" class="primary">Reprocess</button>
    <button type="button" onclick={() => (reprocessOpen = false)}>Cancel</button>
  </form>
</Dialog>

<Dialog
  bind:open={eraseOpen}
  title="Erase this message?"
  description="The message and all its contents are deleted permanently, for example to honor an erasure request. The erasure and your reason are recorded in the audit log."
>
  <form class="stack" onsubmit={erase} novalidate>
    <div class="field">
      <label for="erase-reason">Reason</label>
      <textarea
        id="erase-reason"
        bind:value={eraseReason}
        aria-describedby="erase-hint"
        aria-invalid={eraseError ? 'true' : undefined}
      ></textarea>
      <span class="hint" id="erase-hint">At least {REASON_MIN} characters.</span>
      {#if eraseError}<span class="error-text" role="alert">{eraseError}</span>{/if}
    </div>
    <div class="row">
      <button type="submit" class="danger solid">Erase permanently</button>
      <button type="button" onclick={() => (eraseOpen = false)}>Cancel</button>
    </div>
  </form>
</Dialog>

<style>
  .id {
    font-size: 1.05rem;
    font-weight: 500;
  }

  .summary {
    display: grid;
    grid-template-columns: minmax(18rem, 2fr) minmax(0, 3fr);
    gap: 1rem;
    align-items: start;
    margin-bottom: 1rem;
  }

  @media (max-width: 1100px) {
    .summary {
      grid-template-columns: minmax(0, 1fr);
    }
  }

  .compare {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(22rem, 1fr));
    gap: 1rem;
    align-items: start;
  }

  .error-text {
    color: var(--bad);
  }

  .error-cell {
    max-width: 18rem;
    overflow-wrap: anywhere;
  }
</style>
