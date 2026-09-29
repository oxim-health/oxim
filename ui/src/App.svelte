<script lang="ts">
  import { tick } from 'svelte';
  import { router } from './lib/router.svelte';
  import { session } from './lib/session.svelte';
  import { isClientPath, isCurrent, NAV, PAGE_PERMISSION } from './lib/routes';
  import { applyTheme, storedTheme, type ThemeChoice } from './lib/theme';
  import Account from './pages/Account.svelte';
  import Audit from './pages/Audit.svelte';
  import ChannelEditor from './pages/ChannelEditor.svelte';
  import Channels from './pages/Channels.svelte';
  import Dashboard from './pages/Dashboard.svelte';
  import Forbidden from './pages/Forbidden.svelte';
  import Login from './pages/Login.svelte';
  import MessageDetail from './pages/MessageDetail.svelte';
  import Messages from './pages/Messages.svelte';
  import NotFound from './pages/NotFound.svelte';
  import Placeholder from './pages/Placeholder.svelte';
  import System from './pages/System.svelte';
  import TableEditor from './pages/TableEditor.svelte';
  import Tables from './pages/Tables.svelte';
  import Tokens from './pages/Tokens.svelte';
  import Users from './pages/Users.svelte';

  void session.restore();

  let theme: ThemeChoice = $state(storedTheme());
  let route = $derived(router.route);
  let allowed = $derived(session.can(PAGE_PERMISSION[route.name]));
  let navItems = $derived(NAV.filter((item) => session.can(item.permission)));

  function firstPage(): string {
    return NAV.find((item) => session.can(item.permission))?.href ?? '/account';
  }

  // Send anonymous visitors to the login page and back afterwards.
  $effect(() => {
    if (session.state === 'anonymous' && route.name !== 'login') {
      const next = router.pathname + router.search;
      router.navigate(next === '/' ? '/login' : `/login?next=${encodeURIComponent(next)}`, {
        replace: true,
      });
    } else if (session.state === 'authenticated' && route.name === 'login') {
      const next = router.query.get('next');
      router.navigate(next && isClientPath(next) && !next.startsWith('/login') ? next : firstPage(), {
        replace: true,
      });
    } else if (session.state === 'authenticated' && route.name === 'dashboard' && !allowed) {
      router.navigate(firstPage(), { replace: true });
    }
  });

  // After navigating, move focus to the new page's heading so keyboard and
  // screen reader users start at the top of the new content.
  $effect(() => {
    const visit = router.visits;
    void route;
    if (visit === 0) return;
    void tick().then(() => document.querySelector<HTMLElement>('#main h1')?.focus());
  });

  $effect(() => applyTheme(theme));

  async function logout() {
    await session.logout();
  }
</script>

{#if session.state === 'unknown'}
  <p class="loading" role="status">Loading…</p>
{:else if session.state === 'anonymous' || route.name === 'login'}
  <Login />
{:else if session.user}
  <a class="skip-link" href="#main">Skip to main content</a>
  <header class="topbar">
    <a class="brand" href="/" aria-label="OXIM home">
      <svg viewBox="0 0 32 32" width="26" height="26" aria-hidden="true">
        <rect width="32" height="32" rx="7" class="brand-bg" />
        <circle cx="16" cy="16" r="8.5" fill="none" class="brand-fg-stroke" stroke-width="3.2" />
        <circle cx="16" cy="16" r="2.6" class="brand-fg" />
      </svg>
      <span>OXIM</span>
    </a>
    <div class="user">
      <label class="theme">
        <span class="visually-hidden">Color theme</span>
        <select bind:value={theme}>
          <option value="system">System theme</option>
          <option value="light">Light theme</option>
          <option value="dark">Dark theme</option>
        </select>
      </label>
      <a href="/account" class="who">
        <span>{session.user.display_name}</span>
        <span class="badge neutral">{session.user.role}</span>
      </a>
      <button type="button" class="small" onclick={logout}>Log out</button>
    </div>
  </header>
  <div class="shell">
    <nav aria-label="Main">
      {#each [['operate', 'Operate'], ['configure', 'Configure'], ['administer', 'Administer']] as [section, heading] (section)}
        {@const items = navItems.filter((item) => item.section === section)}
        {#if items.length > 0}
          <h2 class="nav-heading" id="nav-{section}">{heading}</h2>
          <ul aria-labelledby="nav-{section}">
            {#each items as item (item.href)}
              <li>
                <a href={item.href} aria-current={isCurrent(item.href, router.pathname) ? 'page' : undefined}>
                  {item.label}
                </a>
              </li>
            {/each}
          </ul>
        {/if}
      {/each}
    </nav>
    <main id="main" tabindex="-1">
      {#if !allowed}
        <Forbidden role={session.user.role} />
      {:else if route.name === 'dashboard'}
        <Dashboard />
      {:else if route.name === 'channels'}
        <Channels />
      {:else if route.name === 'channel-new'}
        <ChannelEditor id={null} />
      {:else if route.name === 'channel'}
        {#key route.params.id}
          <ChannelEditor id={route.params.id ?? null} />
        {/key}
      {:else if route.name === 'messages'}
        <Messages />
      {:else if route.name === 'message'}
        {#key route.params.id}
          <MessageDetail id={route.params.id ?? ''} />
        {/key}
      {:else if route.name === 'tables'}
        <Tables />
      {:else if route.name === 'table'}
        {#key route.params.name}
          <TableEditor name={route.params.name ?? ''} />
        {/key}
      {:else if route.name === 'users'}
        <Users />
      {:else if route.name === 'tokens'}
        <Tokens />
      {:else if route.name === 'audit'}
        <Audit />
      {:else if route.name === 'system'}
        <System />
      {:else if route.name === 'account'}
        <Account />
      {:else if route.name === 'alerts'}
        <Placeholder
          title="Alerts"
          summary="Alert rules and notifications (queue depth, error rate, device silence, disk space, certificate expiry)."
        />
      {:else if route.name === 'devices'}
        <Placeholder
          title="Devices"
          summary="The device registry: connected analyzers and instruments with status, last message time and firmware."
        />
      {:else if route.name === 'backups'}
        <Placeholder title="Backups" summary="Backup and restore of the message store and configuration." />
      {:else}
        <NotFound />
      {/if}
    </main>
  </div>
{/if}

<style>
  .loading {
    padding: 2rem;
  }

  .topbar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 1rem;
    height: 3.25rem;
    padding: 0 1rem;
    background: var(--surface);
    border-bottom: 1px solid var(--border);
    position: sticky;
    top: 0;
    z-index: 10;
  }

  .brand {
    display: inline-flex;
    align-items: center;
    gap: 0.5rem;
    color: var(--text);
    text-decoration: none;
    font-weight: 700;
    letter-spacing: 0.06em;
  }

  .brand-bg {
    fill: var(--accent);
  }

  .brand-fg {
    fill: var(--accent-text);
  }

  .brand-fg-stroke {
    stroke: var(--accent-text);
  }

  .user {
    display: flex;
    align-items: center;
    gap: 0.75rem;
  }

  .theme select {
    font-size: 0.85rem;
    padding: 0.25rem 0.4rem;
  }

  .who {
    display: inline-flex;
    align-items: center;
    gap: 0.45rem;
    color: var(--text);
    text-decoration: none;
    font-weight: 550;
  }

  .shell {
    display: grid;
    grid-template-columns: 13.5rem minmax(0, 1fr);
    min-height: calc(100vh - 3.25rem);
  }

  nav {
    background: var(--surface);
    border-right: 1px solid var(--border);
    padding: 0.75rem 0.6rem;
  }

  .nav-heading {
    font-size: 0.72rem;
    text-transform: uppercase;
    letter-spacing: 0.08em;
    color: var(--muted);
    margin: 0.9rem 0.5rem 0.3rem;
  }

  .nav-heading:first-child {
    margin-top: 0.25rem;
  }

  nav ul {
    list-style: none;
    margin: 0;
    padding: 0;
  }

  nav a {
    display: block;
    padding: 0.38rem 0.6rem;
    border-radius: var(--radius);
    color: var(--text);
    text-decoration: none;
    font-size: 0.92rem;
  }

  nav a:hover {
    background: var(--surface-2);
  }

  nav a[aria-current='page'] {
    background: var(--accent-soft);
    color: var(--text);
    font-weight: 650;
    box-shadow: inset 3px 0 0 var(--accent);
  }

  main {
    padding: 1.25rem 1.5rem 3rem;
    min-width: 0;
  }

  main:focus {
    outline: none;
  }

  @media (max-width: 1024px) {
    .shell {
      grid-template-columns: minmax(0, 1fr);
    }

    nav {
      border-right: none;
      border-bottom: 1px solid var(--border);
      display: flex;
      flex-wrap: wrap;
      gap: 0.25rem 1rem;
      align-items: center;
    }

    .nav-heading {
      display: none;
    }

    nav ul {
      display: flex;
      flex-wrap: wrap;
      gap: 0.25rem;
    }

    main {
      padding: 1rem;
    }
  }
</style>
