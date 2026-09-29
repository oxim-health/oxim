<script lang="ts">
  import { onMount } from 'svelte';
  import Dialog from '../lib/components/Dialog.svelte';
  import ErrorNotice from '../lib/components/ErrorNotice.svelte';
  import StatusBadge from '../lib/components/StatusBadge.svelte';
  import { api, isBroken, type ChannelEntry } from '../lib/api';
  import { formatAge, formatCount } from '../lib/format';
  import { session } from '../lib/session.svelte';

  let channels = $state<ChannelEntry[] | null>(null);
  let error = $state<unknown>(null);
  let message = $state<string | null>(null);
  let busy = $state<string | null>(null);
  let deleting = $state<string | null>(null);
  let deleteOpen = $state(false);

  async function load() {
    try {
      channels = (await api.channels()).channels;
      error = null;
    } catch (failure) {
      error = failure;
    }
  }

  onMount(load);

  async function act(id: string, action: 'deploy' | 'undeploy' | 'redeploy') {
    busy = `${id}:${action}`;
    error = null;
    message = null;
    try {
      await api[action](id);
      message = `Channel ${id}: ${action === 'deploy' ? 'deployed' : action === 'undeploy' ? 'undeployed' : 'redeployed'}.`;
      await load();
    } catch (failure) {
      error = failure;
    } finally {
      busy = null;
    }
  }

  function askDelete(id: string) {
    deleting = id;
    deleteOpen = true;
  }

  async function confirmDelete(event: SubmitEvent) {
    event.preventDefault();
    if (!deleting) return;
    const id = deleting;
    deleteOpen = false;
    try {
      await api.deleteChannel(id);
      message = `Channel ${id} was undeployed and its file moved to .deleted/.`;
      await load();
    } catch (failure) {
      error = failure;
    }
  }

  function queued(entry: ChannelEntry): number {
    if (isBroken(entry)) return 0;
    return entry.destinations.reduce(
      (sum, destination) => sum + (destination.queue ? destination.queue.queued + destination.queue.retrying + destination.queue.sending : 0),
      0,
    );
  }

  function failed(entry: ChannelEntry): number {
    if (isBroken(entry)) return 0;
    return entry.destinations.reduce((sum, destination) => sum + (destination.queue?.failed ?? 0), 0);
  }

  function oldest(entry: ChannelEntry): string | null {
    if (isBroken(entry)) return null;
    return (
      entry.destinations
        .map((destination) => destination.queue?.oldest_pending_at ?? null)
        .filter((value): value is string => value !== null)
        .sort()[0] ?? null
    );
  }
</script>

<svelte:head><title>Channels · OXIM</title></svelte:head>

<div class="page-header">
  <div>
    <h1 tabindex="-1">Channels</h1>
    <p>Channel files in the channel directory, their deployment state and queues.</p>
  </div>
  {#if session.can('edit_channels')}
    <a class="button primary" href="/channels/new">New channel</a>
  {/if}
</div>

<ErrorNotice {error} />
{#if message}<div class="notice success" role="status"><p>{message}</p></div>{/if}

{#if channels === null}
  <p class="muted" role="status">Loading channels…</p>
{:else if channels.length === 0}
  <div class="panel">
    <p>No channel files yet.</p>
    {#if session.can('edit_channels')}<p><a href="/channels/new">Create the first channel</a>.</p>{/if}
  </div>
{:else}
  <div class="table-wrap">
    <table>
      <caption class="visually-hidden">Channels</caption>
      <thead>
        <tr>
          <th scope="col">Channel</th>
          <th scope="col">State</th>
          <th scope="col">Source</th>
          <th scope="col">Destinations</th>
          <th scope="col" class="num">Queued</th>
          <th scope="col" class="num">Failed</th>
          <th scope="col">Oldest waiting</th>
          <th scope="col"><span class="visually-hidden">Actions</span></th>
        </tr>
      </thead>
      <tbody>
        {#each channels as entry (entry.file)}
          {#if isBroken(entry)}
            <tr>
              <th scope="row">{entry.file}</th>
              <td><StatusBadge status="invalid" /></td>
              <td colspan="6"><span class="error-text">{entry.error}</span></td>
            </tr>
          {:else}
            <tr>
              <th scope="row">
                <a href="/channels/{encodeURIComponent(entry.id)}">{entry.id}</a>
                {#if entry.name}<div class="muted small-text">{entry.name}</div>{/if}
              </th>
              <td>
                <StatusBadge status={entry.deployed ? 'deployed' : 'stopped'} />
                {#if !entry.enabled}<StatusBadge status="disabled" />{/if}
              </td>
              <td>
                <span class="mono">{entry.source.type}</span>
                <div class="muted small-text">{entry.source.data_type}</div>
              </td>
              <td>
                {#each entry.destinations as destination (destination.id)}
                  <div><span class="mono">{destination.id}</span> <span class="muted small-text">{destination.type}</span></div>
                {/each}
              </td>
              <td class="num">{formatCount(queued(entry))}</td>
              <td class="num">
                {#if failed(entry) > 0}<span class="badge bad">{formatCount(failed(entry))} failed</span>{:else}0{/if}
              </td>
              <td class="nowrap">{formatAge(oldest(entry))}</td>
              <td class="actions">
                {#if session.can('deploy_channels')}
                  {#if entry.deployed}
                    <button
                      type="button"
                      class="small"
                      disabled={busy !== null}
                      onclick={() => act(entry.id, 'redeploy')}
                      aria-label="Redeploy {entry.id}">Redeploy</button
                    >
                    <button
                      type="button"
                      class="small"
                      disabled={busy !== null}
                      onclick={() => act(entry.id, 'undeploy')}
                      aria-label="Undeploy {entry.id}">Undeploy</button
                    >
                  {:else}
                    <button
                      type="button"
                      class="small"
                      disabled={busy !== null}
                      onclick={() => act(entry.id, 'deploy')}
                      aria-label="Deploy {entry.id}">Deploy</button
                    >
                  {/if}
                {/if}
                {#if session.can('edit_channels') && session.can('deploy_channels')}
                  <button type="button" class="small danger" onclick={() => askDelete(entry.id)} aria-label="Delete {entry.id}"
                    >Delete</button
                  >
                {/if}
              </td>
            </tr>
          {/if}
        {/each}
      </tbody>
    </table>
  </div>
{/if}

<Dialog bind:open={deleteOpen} title="Delete channel {deleting ?? ''}?" description="The channel is undeployed and its file is moved to the .deleted/ folder of the channel directory. Stored messages are kept.">
  <form class="row" onsubmit={confirmDelete}>
    <button type="submit" class="danger solid">Delete channel</button>
    <button type="button" onclick={() => (deleteOpen = false)}>Cancel</button>
  </form>
</Dialog>

<style>
  .actions {
    white-space: nowrap;
    text-align: right;
  }

  .actions button + button {
    margin-left: 0.3rem;
  }

  .error-text {
    color: var(--bad);
  }

  th[scope='row'] {
    background: none;
    position: static;
  }
</style>
