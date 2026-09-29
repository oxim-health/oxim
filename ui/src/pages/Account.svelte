<script lang="ts">
  import ErrorNotice from '../lib/components/ErrorNotice.svelte';
  import { api } from '../lib/api';
  import { permissionLabel } from '../lib/format';
  import { session } from '../lib/session.svelte';

  const MIN_LENGTH = 12;

  let current = $state('');
  let next = $state('');
  let repeat = $state('');
  let busy = $state(false);
  let error = $state<unknown>(null);
  let problem = $derived.by(() => {
    if (!next) return null;
    if (next.length < MIN_LENGTH) return `Use at least ${MIN_LENGTH} characters.`;
    if (session.user && next.toLowerCase() === session.user.username.toLowerCase()) {
      return 'The password must differ from the user name.';
    }
    if (repeat && repeat !== next) return 'The two new passwords do not match.';
    return null;
  });

  async function submit(event: SubmitEvent) {
    event.preventDefault();
    if (problem || !current || !next || next !== repeat) {
      error = new Error(problem ?? 'Fill in every field.');
      return;
    }
    busy = true;
    error = null;
    try {
      await api.changePassword(current, next);
      // The server ends every session of the user, including this one.
      session.expire('Your password was changed. Log in with the new password.');
    } catch (failure) {
      error = failure;
    } finally {
      busy = false;
    }
  }
</script>

<svelte:head><title>Account · OXIM</title></svelte:head>

<div class="page-header">
  <div>
    <h1 tabindex="-1">Account</h1>
    <p>Your user details and password.</p>
  </div>
</div>

{#if session.user}
  <div class="columns">
    <section class="panel" aria-labelledby="who-heading">
      <h2 id="who-heading">Signed in as</h2>
      <dl class="facts">
        <dt>User name</dt>
        <dd>{session.user.username}</dd>
        <dt>Name</dt>
        <dd>{session.user.display_name}</dd>
        <dt>Role</dt>
        <dd>{session.user.role}</dd>
        <dt>Permissions</dt>
        <dd>
          <ul class="permissions">
            {#each session.user.permissions as permission (permission)}
              <li>{permissionLabel(permission)}</li>
            {/each}
          </ul>
        </dd>
      </dl>
    </section>

    <section class="panel" aria-labelledby="password-heading">
      <h2 id="password-heading">Change password</h2>
      {#if session.interactive}
        <form class="stack" onsubmit={submit} novalidate>
          <ErrorNotice {error} title="The password was not changed" />
          <div class="field">
            <label for="current-password">Current password</label>
            <input id="current-password" type="password" autocomplete="current-password" bind:value={current} />
          </div>
          <div class="field">
            <label for="new-password">New password</label>
            <input
              id="new-password"
              type="password"
              autocomplete="new-password"
              bind:value={next}
              aria-describedby="new-password-hint"
              aria-invalid={problem ? 'true' : undefined}
            />
            <span class="hint" id="new-password-hint">
              At least {MIN_LENGTH} characters and different from your user name. Every session,
              including this one, ends after the change.
            </span>
          </div>
          <div class="field">
            <label for="repeat-password">Repeat the new password</label>
            <input id="repeat-password" type="password" autocomplete="new-password" bind:value={repeat} />
          </div>
          {#if problem}<p class="error-text" role="status">{problem}</p>{/if}
          <div class="row">
            <button class="primary" type="submit" disabled={busy}>Change password</button>
          </div>
        </form>
      {:else}
        <p class="muted">API tokens have no password.</p>
      {/if}
    </section>
  </div>
{/if}

<style>
  .columns {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(20rem, 1fr));
    gap: 1rem;
    align-items: start;
  }

  .permissions {
    margin: 0;
    padding-left: 1.1rem;
    columns: 2;
  }

  .error-text {
    color: var(--bad);
    margin: 0;
  }
</style>
