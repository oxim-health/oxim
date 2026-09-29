<script lang="ts">
  import { onMount } from 'svelte';
  import Dialog from '../lib/components/Dialog.svelte';
  import ErrorNotice from '../lib/components/ErrorNotice.svelte';
  import StatusBadge from '../lib/components/StatusBadge.svelte';
  import Time from '../lib/components/Time.svelte';
  import { api, ApiError, ROLES, type ApiToken, type Role } from '../lib/api';
  import { session } from '../lib/session.svelte';

  let tokens = $state<ApiToken[] | null>(null);
  let error = $state<unknown>(null);
  let notice = $state<string | null>(null);

  let createOpen = $state(false);
  let draft = $state({ name: '', role: 'viewer' as Role, expires: '' });
  let createError = $state<string | null>(null);

  let secret = $state<{ name: string; token: string } | null>(null);
  let secretOpen = $state(false);
  let copied = $state(false);

  let revoking = $state<ApiToken | null>(null);
  let revokeOpen = $state(false);

  // A token cannot have more permissions than its creator.
  const allowedRoles = $derived(
    ROLES.filter((role) => ROLES.indexOf(role) >= ROLES.indexOf(session.user?.role ?? 'viewer')),
  );

  async function load() {
    try {
      tokens = (await api.tokens()).tokens;
    } catch (failure) {
      error = failure;
      tokens = [];
    }
  }

  onMount(load);

  async function create(event: SubmitEvent) {
    event.preventDefault();
    const name = draft.name.trim();
    if (!name) {
      createError = 'Name the token after what uses it, for example "prometheus".';
      return;
    }
    let expires_at: string | null = null;
    if (draft.expires) {
      const date = new Date(`${draft.expires}T23:59:59`);
      if (Number.isNaN(date.getTime()) || date.getTime() <= Date.now()) {
        createError = 'The expiry date must be in the future.';
        return;
      }
      expires_at = date.toISOString();
    }
    try {
      const created = await api.createToken({ name, role: draft.role, expires_at });
      createOpen = false;
      draft = { name: '', role: 'viewer', expires: '' };
      secret = { name: created.name, token: created.token };
      copied = false;
      secretOpen = true;
      await load();
    } catch (failure) {
      createError = failure instanceof ApiError ? failure.message : 'The token was not created.';
    }
  }

  async function copy() {
    if (!secret) return;
    try {
      await navigator.clipboard.writeText(secret.token);
      copied = true;
    } catch {
      copied = false;
    }
  }

  async function revoke(event: SubmitEvent) {
    event.preventDefault();
    if (!revoking) return;
    const target = revoking;
    revokeOpen = false;
    try {
      await api.revokeToken(target.id);
      notice = `Token ${target.name} was revoked.`;
      await load();
    } catch (failure) {
      error = failure;
    }
  }
</script>

<svelte:head><title>API tokens · OXIM</title></svelte:head>

<div class="page-header">
  <div>
    <h1 tabindex="-1">API tokens</h1>
    <p>Credentials for scripts and monitoring (for example Prometheus at <span class="mono">/metrics</span>).</p>
  </div>
  <button
    type="button"
    class="primary"
    onclick={() => {
      createError = null;
      createOpen = true;
    }}>Create token</button
  >
</div>

<ErrorNotice {error} />
{#if notice}<div class="notice success" role="status"><p>{notice}</p></div>{/if}

{#if tokens === null}
  <p class="muted" role="status">Loading tokens…</p>
{:else if tokens.length === 0}
  <p class="muted">No API tokens.</p>
{:else}
  <div class="table-wrap">
    <table>
      <caption class="visually-hidden">API tokens</caption>
      <thead>
        <tr>
          <th scope="col">Name</th>
          <th scope="col">Role</th>
          <th scope="col">State</th>
          <th scope="col">Created</th>
          <th scope="col">Expires</th>
          <th scope="col">Last used</th>
          <th scope="col"><span class="visually-hidden">Actions</span></th>
        </tr>
      </thead>
      <tbody>
        {#each tokens as token (token.id)}
          <tr>
            <td>{token.name}<div class="muted small-text">by {token.created_by}</div></td>
            <td>{token.role}</td>
            <td><StatusBadge status={token.revoked ? 'revoked' : 'active'} /></td>
            <td class="nowrap"><Time value={token.created_at} /></td>
            <td class="nowrap"><Time value={token.expires_at} /></td>
            <td class="nowrap"><Time value={token.last_used_at} /></td>
            <td>
              {#if !token.revoked}
                <button
                  type="button"
                  class="small danger"
                  onclick={() => {
                    revoking = token;
                    revokeOpen = true;
                  }}
                  aria-label="Revoke {token.name}">Revoke</button
                >
              {/if}
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
  </div>
{/if}

<Dialog bind:open={createOpen} title="Create API token">
  <form class="stack" onsubmit={create} novalidate>
    <div class="field">
      <label for="token-name">Name</label>
      <input id="token-name" bind:value={draft.name} autocomplete="off" />
    </div>
    <div class="field">
      <label for="token-role">Role</label>
      <select id="token-role" bind:value={draft.role}>
        {#each allowedRoles as role (role)}<option value={role}>{role}</option>{/each}
      </select>
      <span class="hint">A token cannot have more permissions than you. Prometheus needs viewer.</span>
    </div>
    <div class="field">
      <label for="token-expires">Expires on</label>
      <input id="token-expires" type="date" bind:value={draft.expires} />
      <span class="hint">Optional. Without a date the token is valid until revoked.</span>
    </div>
    {#if createError}<p class="error-text" role="alert">{createError}</p>{/if}
    <div class="row">
      <button type="submit" class="primary">Create token</button>
      <button type="button" onclick={() => (createOpen = false)}>Cancel</button>
    </div>
  </form>
</Dialog>

<Dialog
  bind:open={secretOpen}
  title="Token {secret?.name ?? ''} created"
  description="Copy the token now. It is shown only once; OXIM stores only a hash of it."
  onclose={() => (secret = null)}
>
  {#if secret}
    <div class="field">
      <label for="token-secret">Token</label>
      <input id="token-secret" class="mono" readonly value={secret.token} />
    </div>
    <p class="small-text">Send it as <span class="mono">Authorization: Bearer &lt;token&gt;</span>.</p>
    <div class="row">
      <button type="button" onclick={copy}>{copied ? 'Copied' : 'Copy to clipboard'}</button>
      <button type="button" class="primary" onclick={() => (secretOpen = false)}>Done</button>
    </div>
    <p class="visually-hidden" role="status">{copied ? 'Token copied to the clipboard.' : ''}</p>
  {/if}
</Dialog>

<Dialog bind:open={revokeOpen} title="Revoke token {revoking?.name ?? ''}?" description="Programs using this token lose access immediately.">
  <form class="row" onsubmit={revoke}>
    <button type="submit" class="danger solid">Revoke</button>
    <button type="button" onclick={() => (revokeOpen = false)}>Cancel</button>
  </form>
</Dialog>

<style>
  .error-text {
    color: var(--bad);
    margin: 0;
  }
</style>
