<script lang="ts">
  import { onMount } from 'svelte';
  import ErrorNotice from '../lib/components/ErrorNotice.svelte';
  import StatusBadge from '../lib/components/StatusBadge.svelte';
  import { api, ApiError, isBroken, type Channel } from '../lib/api';
  import { channelTemplate, locate, readChannel, type Step, type YamlProblem } from '../lib/channel-yaml';
  import { router } from '../lib/router.svelte';
  import { session } from '../lib/session.svelte';

  let { id }: { id: string | null } = $props();

  // CodeMirror is most of the bundle, so it loads with this page only.
  const loadEditor = () => import('../lib/components/CodeEditor.svelte').then((module) => module.default);

  const editor = session.can('edit_channels');
  let creating = $derived(id === null);
  let yaml = $state('');
  let saved = $state('');
  let file = $state<string | null>(null);
  let deployed = $state(false);
  let summary = $state<Channel | null>(null);
  let loading = $state(true);
  let error = $state<unknown>(null);
  let serverProblem = $state<YamlProblem | null>(null);
  let message = $state<string | null>(null);
  let busy = $state(false);

  // Parse while typing, with a short delay.
  let parsed = $state(readChannel(''));
  $effect(() => {
    const text = yaml;
    const timer = setTimeout(() => (parsed = readChannel(text)), 250);
    return () => clearTimeout(timer);
  });
  let problems = $derived(serverProblem ? [serverProblem, ...parsed.problems] : parsed.problems);
  let dirty = $derived(yaml !== saved);

  onMount(() => {
    const warn = (event: BeforeUnloadEvent) => {
      if (dirty) event.preventDefault();
    };
    window.addEventListener('beforeunload', warn);
    void load();
    return () => window.removeEventListener('beforeunload', warn);
  });

  async function load() {
    loading = true;
    try {
      if (id === null) {
        yaml = channelTemplate('new-channel');
        saved = '';
      } else if (editor) {
        const channel = await api.channel(id);
        yaml = channel.yaml;
        saved = channel.yaml;
        file = channel.file;
        deployed = channel.deployed;
      } else {
        const list = await api.channels();
        summary = (list.channels.find((entry) => !isBroken(entry) && entry.id === id) as Channel | undefined) ?? null;
        if (!summary) error = new Error(`Channel ${id} not found.`);
      }
    } catch (failure) {
      error = failure;
    } finally {
      loading = false;
    }
  }

  async function save(event: SubmitEvent) {
    event.preventDefault();
    const current = readChannel(yaml);
    const target = id ?? current.view?.id ?? '';
    if (!target) {
      error = new Error('Set the channel id in the YAML (id: my-channel).');
      return;
    }
    if (current.view && current.view.id && current.view.id !== target) {
      error = new Error(`The YAML defines channel ${current.view.id}; this page edits ${target}.`);
      return;
    }
    busy = true;
    error = null;
    serverProblem = null;
    message = null;
    try {
      const result = await api.saveChannel(target, yaml);
      saved = yaml;
      file = result.file;
      message = deployed
        ? `Saved to ${result.file}. The running channel picks up the change when OXIM next checks the channel directory; use Redeploy to apply it now.`
        : `Saved to ${result.file}.`;
      if (id === null) router.navigate(`/channels/${encodeURIComponent(target)}`, { replace: true });
    } catch (failure) {
      error = failure;
      if (failure instanceof ApiError && failure.code === 'invalid_channel') {
        const where = locate(failure.message);
        serverProblem = { message: failure.message, ...where };
      }
    } finally {
      busy = false;
    }
  }

  async function act(action: 'deploy' | 'undeploy' | 'redeploy') {
    if (!id) return;
    busy = true;
    error = null;
    message = null;
    try {
      await api[action](id);
      deployed = action !== 'undeploy';
      message = `Channel ${id} ${action === 'deploy' ? 'deployed' : action === 'undeploy' ? 'undeployed' : 'redeployed'}.`;
    } catch (failure) {
      error = failure;
    } finally {
      busy = false;
    }
  }
</script>

{#snippet steps(label: string, list: Step[])}
  {#if list.length > 0}
    <h4>{label}</h4>
    <ol class="steps">
      {#each list as step, index (index)}
        <li>
          <span class="mono">{step.type}</span>
          {#if step.settings.length > 0}
            <dl class="settings">
              {#each step.settings as [key, value] (key)}
                <dt>{key}</dt>
                <dd class="mono">{value}</dd>
              {/each}
            </dl>
          {/if}
        </li>
      {/each}
    </ol>
  {/if}
{/snippet}

{#snippet settings(list: [string, string][])}
  {#if list.length > 0}
    <dl class="settings">
      {#each list as [key, value] (key)}
        <dt>{key}</dt>
        <dd class="mono">{value}</dd>
      {/each}
    </dl>
  {/if}
{/snippet}

<svelte:head><title>{creating ? 'New channel' : `Channel ${id}`} · OXIM</title></svelte:head>

<div class="page-header">
  <div>
    <h1 tabindex="-1">{creating ? 'New channel' : `Channel ${id}`}</h1>
    <p>
      {#if creating}
        Write the channel in YAML; OXIM validates it against the installed components before saving.
      {:else if file}
        File <span class="mono">{file}</span>
      {:else}
        Channel configuration
      {/if}
    </p>
  </div>
  {#if !creating && session.can('deploy_channels')}
    <div class="row">
      <StatusBadge status={deployed || summary?.deployed ? 'deployed' : 'stopped'} />
      {#if deployed || summary?.deployed}
        <button type="button" onclick={() => act('redeploy')} disabled={busy || dirty}>Redeploy</button>
        <button type="button" onclick={() => act('undeploy')} disabled={busy}>Undeploy</button>
      {:else}
        <button type="button" onclick={() => act('deploy')} disabled={busy || dirty}>Deploy</button>
      {/if}
    </div>
  {/if}
</div>

<p><a href="/channels">Back to channels</a></p>

<ErrorNotice {error} title="The channel was not saved" />
{#if message}<div class="notice success" role="status"><p>{message}</p></div>{/if}

{#if loading}
  <p class="muted" role="status">Loading…</p>
{:else if editor}
  <div class="layout">
    <form class="panel editor-panel stack" onsubmit={save}>
      <div class="spread">
        <h2 id="yaml-heading">Channel file</h2>
        <div class="row">
          {#if dirty}<span class="badge warn">Unsaved changes</span>{/if}
          <button type="button" onclick={() => (yaml = saved)} disabled={!dirty || busy || creating}>Revert</button>
          <button type="submit" class="primary" disabled={busy || (!dirty && !creating)}>Save</button>
        </div>
      </div>
      {#await loadEditor()}
        <p class="muted" role="status">Loading the editor…</p>
      {:then CodeEditor}
        <CodeEditor bind:value={yaml} label="Channel YAML" {problems} />
      {:catch}
        <p class="error-text" role="alert">The editor could not be loaded; reload the page.</p>
      {/await}
      <div id="yaml-problems" aria-live="polite">
        {#if problems.length > 0}
          <ul class="problems">
            {#each problems as problem, index (index)}
              <li>
                {#if problem.line}<span class="mono">Line {problem.line}{problem.column ? `:${problem.column}` : ''}</span> —{/if}
                {problem.message}
              </li>
            {/each}
          </ul>
        {/if}
      </div>
    </form>

    <section class="panel structure" aria-labelledby="structure-heading">
      <h2 id="structure-heading">Structure</h2>
      {#if parsed.view}
        {@const view = parsed.view}
        <dl class="facts">
          <dt>Id</dt>
          <dd class="mono">{view.id || '—'}</dd>
          {#if view.name}<dt>Name</dt><dd>{view.name}</dd>{/if}
          <dt>Enabled</dt>
          <dd>{view.enabled ? 'Yes' : 'No'}</dd>
        </dl>
        <h3>Source</h3>
        <p>
          <span class="mono">{view.source.type || '(no type)'}</span> receiving
          <span class="mono">{view.source.dataType || '(no data type)'}</span>
          {#if view.source.normalize}<span class="badge info">normalized</span>{/if}
        </p>
        {@render settings(view.source.settings)}
        {#if view.source.response.length > 0}
          <h4>Replies</h4>
          {@render settings(view.source.response)}
        {/if}
        {@render steps('Filters', view.filters)}
        {@render steps('Transformers', view.transformers)}
        <h3>Destinations</h3>
        {#if view.destinations.length === 0}
          <p class="muted">None: messages are stored only.</p>
        {/if}
        {#each view.destinations as destination (destination.id)}
          <div class="destination">
            <p><span class="mono">{destination.id}</span> <span class="muted">({destination.type})</span></p>
            {@render settings(destination.settings)}
            {@render steps('Filters', destination.filters)}
            {@render steps('Transformers', destination.transformers)}
            {#if destination.encoder}{@render steps('Encoder', [destination.encoder])}{/if}
            {#if destination.queue.length > 0}
              <h4>Queue</h4>
              {@render settings(destination.queue)}
            {/if}
          </div>
        {/each}
      {:else}
        <p class="muted">The YAML cannot be read; see the problems below the editor.</p>
      {/if}
    </section>
  </div>
{:else if summary}
  <section class="panel" aria-labelledby="summary-heading">
    <h2 id="summary-heading">Summary</h2>
    <p class="muted">
      Only users who may edit channels can read channel files, because they may contain credentials.
    </p>
    <dl class="facts">
      <dt>Name</dt>
      <dd>{summary.name ?? '—'}</dd>
      <dt>Source</dt>
      <dd><span class="mono">{summary.source.type}</span> ({summary.source.data_type})</dd>
      <dt>Destinations</dt>
      <dd>
        {#each summary.destinations as destination (destination.id)}
          <div><span class="mono">{destination.id}</span> ({destination.type})</div>
        {/each}
      </dd>
    </dl>
  </section>
{/if}

<style>
  .layout {
    display: grid;
    grid-template-columns: minmax(0, 3fr) minmax(16rem, 2fr);
    gap: 1rem;
    align-items: start;
  }

  @media (max-width: 1100px) {
    .layout {
      grid-template-columns: minmax(0, 1fr);
    }
  }

  .error-text {
    color: var(--bad);
  }

  .problems {
    margin: 0;
    padding-left: 1.2rem;
    color: var(--bad);
  }

  .structure h3 {
    margin-top: 1rem;
  }

  .structure h4 {
    margin: 0.6rem 0 0.25rem;
    font-size: 0.85rem;
  }

  .structure p {
    margin: 0.25rem 0;
  }

  .steps {
    margin: 0;
    padding-left: 1.3rem;
  }

  .settings {
    display: grid;
    grid-template-columns: max-content 1fr;
    gap: 0.1rem 0.75rem;
    margin: 0.2rem 0 0.4rem;
    font-size: 0.84rem;
  }

  .settings dt {
    color: var(--muted);
  }

  .settings dd {
    margin: 0;
    overflow-wrap: anywhere;
  }

  .destination {
    border-top: 1px solid var(--border);
    padding-top: 0.5rem;
    margin-top: 0.5rem;
  }
</style>
