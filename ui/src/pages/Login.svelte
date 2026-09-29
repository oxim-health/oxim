<script lang="ts">
  import { ApiError } from '../lib/api';
  import { session } from '../lib/session.svelte';

  let username = $state('');
  let password = $state('');
  let busy = $state(false);
  let error = $state<string | null>(null);
  let heading: HTMLHeadingElement | undefined = $state();

  $effect(() => heading?.focus());

  async function submit(event: SubmitEvent) {
    event.preventDefault();
    if (!username || !password) {
      error = 'Enter your user name and password.';
      return;
    }
    busy = true;
    error = null;
    try {
      await session.login(username, password);
    } catch (failure) {
      if (failure instanceof ApiError && failure.status === 429) {
        const wait = failure.retryAfter ? ` Try again in ${Math.ceil(failure.retryAfter / 60)} minute(s).` : '';
        error = `Too many failed logins.${wait}`;
      } else if (failure instanceof ApiError && failure.status === 401) {
        error = 'The user name or password is not correct, or the account is disabled.';
      } else {
        error = failure instanceof Error ? failure.message : 'Login failed.';
      }
      password = '';
    } finally {
      busy = false;
    }
  }
</script>

<svelte:head><title>Log in · OXIM</title></svelte:head>

<main class="login" id="main">
  <div class="card panel">
    <div class="brand" aria-hidden="true">OXIM</div>
    <h1 bind:this={heading} tabindex="-1">Log in</h1>
    <p class="muted">Open eXchange for Interoperable Medicine</p>
    {#if session.notice}
      <div class="notice" role="status"><p>{session.notice}</p></div>
    {/if}
    {#if error}
      <div class="notice error" role="alert" id="login-error"><p>{error}</p></div>
    {/if}
    <form class="stack" onsubmit={submit} novalidate>
      <div class="field">
        <label for="username">User name</label>
        <input
          id="username"
          name="username"
          autocomplete="username"
          bind:value={username}
          required
          aria-invalid={error ? 'true' : undefined}
          aria-describedby={error ? 'login-error' : undefined}
        />
      </div>
      <div class="field">
        <label for="password">Password</label>
        <input
          id="password"
          name="password"
          type="password"
          autocomplete="current-password"
          bind:value={password}
          required
          aria-invalid={error ? 'true' : undefined}
          aria-describedby={error ? 'login-error' : undefined}
        />
      </div>
      <button class="primary" type="submit" disabled={busy}>{busy ? 'Logging in…' : 'Log in'}</button>
    </form>
    <p class="muted small-text">
      Accounts are created by an administrator. The first administrator is created on the server
      with <code>oxim users create-admin</code>.
    </p>
  </div>
</main>

<style>
  .login {
    min-height: 100vh;
    display: grid;
    place-items: center;
    padding: 1rem;
  }

  .card {
    width: min(24rem, 100%);
    display: flex;
    flex-direction: column;
    gap: 0.75rem;
  }

  .brand {
    font-weight: 800;
    letter-spacing: 0.12em;
    color: var(--accent);
  }

  h1 {
    margin: 0;
  }

  p {
    margin: 0;
  }

  button {
    justify-content: center;
  }
</style>
