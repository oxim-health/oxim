<script lang="ts">
  import ErrorNotice from '../lib/components/ErrorNotice.svelte';
  import Time from '../lib/components/Time.svelte';
  import { api, type AuditEvent } from '../lib/api';
  import { withQuery } from '../lib/api/client';
  import { router } from '../lib/router.svelte';

  let events = $state<AuditEvent[] | null>(null);
  let error = $state<unknown>(null);
  let form = $state({ action: '', actor: '', message: '', limit: '200' });
  let query = $derived(router.pathname === '/audit' ? router.query : new URLSearchParams());

  $effect(() => {
    form = {
      action: query.get('action') ?? '',
      actor: query.get('actor') ?? '',
      message: query.get('message') ?? '',
      limit: query.get('limit') ?? '200',
    };
    void load(query);
  });

  let ticket = 0;
  async function load(params: URLSearchParams) {
    const current = ++ticket;
    events = null;
    try {
      const result = await api.audit({
        action: params.get('action'),
        actor: params.get('actor'),
        message: params.get('message'),
        limit: params.get('limit') ?? '200',
      });
      if (current === ticket) {
        events = result.events;
        error = null;
      }
    } catch (failure) {
      if (current === ticket) {
        error = failure;
        events = [];
      }
    }
  }

  function submit(event: SubmitEvent) {
    event.preventDefault();
    router.navigate(
      withQuery('/audit', {
        action: form.action.trim(),
        actor: form.actor.trim(),
        message: form.message.trim(),
        limit: form.limit === '200' ? undefined : form.limit,
      }),
    );
  }
</script>

<svelte:head><title>Audit log · OXIM</title></svelte:head>

<div class="page-header">
  <div>
    <h1 tabindex="-1">Audit log</h1>
    <p>Who did what: logins, content views, break-glass access, repairs and configuration changes.</p>
  </div>
</div>

<form class="panel filters" onsubmit={submit} aria-label="Audit filters">
  <div class="form-grid">
    <div class="field">
      <label for="audit-action">Action starts with</label>
      <input id="audit-action" bind:value={form.action} placeholder="message." />
    </div>
    <div class="field">
      <label for="audit-actor">Actor</label>
      <input id="audit-actor" bind:value={form.actor} placeholder="user name or token:name" />
    </div>
    <div class="field">
      <label for="audit-message">Message id</label>
      <input id="audit-message" class="mono" bind:value={form.message} />
    </div>
    <div class="field">
      <label for="audit-limit">Show</label>
      <select id="audit-limit" bind:value={form.limit}>
        <option value="100">100 events</option>
        <option value="200">200 events</option>
        <option value="1000">1,000 events</option>
        <option value="5000">5,000 events</option>
      </select>
    </div>
  </div>
  <div class="row buttons">
    <button type="submit" class="primary">Search</button>
  </div>
</form>

<ErrorNotice {error} />

{#if events === null}
  <p class="muted" role="status">Loading audit events…</p>
{:else if events.length === 0}
  <p class="muted" role="status">No audit events match.</p>
{:else}
  <div class="table-wrap">
    <table>
      <caption class="visually-hidden">Audit events, newest first</caption>
      <thead>
        <tr>
          <th scope="col">Time</th>
          <th scope="col">Actor</th>
          <th scope="col">Action</th>
          <th scope="col">Channel</th>
          <th scope="col">Message</th>
          <th scope="col">Detail</th>
        </tr>
      </thead>
      <tbody>
        {#each events as event, index (index)}
          <tr>
            <td class="nowrap"><Time value={event.at} /></td>
            <td>{event.actor}</td>
            <td class="mono">{event.action}</td>
            <td>{event.channel ?? '—'}</td>
            <td>
              {#if event.message_id}<a class="mono small-text" href="/messages/{event.message_id}">{event.message_id}</a>{:else}—{/if}
            </td>
            <td class="small-text detail">{event.detail ?? ''}</td>
          </tr>
        {/each}
      </tbody>
    </table>
  </div>
{/if}

<style>
  .filters {
    margin-bottom: 1rem;
  }

  .buttons {
    margin-top: 0.75rem;
  }

  .detail {
    max-width: 28rem;
    overflow-wrap: anywhere;
  }
</style>
