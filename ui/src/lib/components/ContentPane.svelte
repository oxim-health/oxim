<script lang="ts">
  // One stored content of a message (raw, normalized, transformed, reply,
  // or a destination's encoded bytes and response), shown as text, as
  // fields, or as a tree. Every load is recorded in the audit log by the
  // server, so content is only fetched for the selected stage.
  import Dialog from './Dialog.svelte';
  import ErrorNotice from './ErrorNotice.svelte';
  import JsonTree from './JsonTree.svelte';
  import { api, ApiError, type Content, type ContentInfo } from '../api';
  import { formatBytes } from '../format';
  import { segmentsFor } from '../segments';
  import { session } from '../session.svelte';

  let {
    messageId,
    contents,
    selected = $bindable(0),
    label,
  }: { messageId: string; contents: ContentInfo[]; selected?: number; label: string } = $props();

  const id = $props.id();
  const REASON_MIN = 8;

  let content = $state<Content | null>(null);
  let error = $state<unknown>(null);
  let loading = $state(false);
  let mode = $state<'text' | 'fields' | 'tree'>('text');
  let breakGlass = $state(false);
  let reason = $state('');
  let reasonError = $state<string | null>(null);
  let unmaskedByBreakGlass = $state(false);

  let info = $derived(contents[selected] ?? null);
  let text = $derived(content?.encoding === 'utf8' && content.data !== null ? content.data : null);
  let segments = $derived(text !== null ? segmentsFor(content?.data_type, text) : null);
  let json = $derived.by(() => {
    if (text === null) return undefined;
    const trimmed = text.trimStart();
    if (!trimmed.startsWith('{') && !trimmed.startsWith('[')) return undefined;
    try {
      return JSON.parse(text) as unknown;
    } catch {
      return undefined;
    }
  });
  let display = $derived(text === null ? '' : text.replace(/\r\n|\r/g, '\n'));

  function describe(item: ContentInfo): string {
    const names: Record<string, string> = {
      raw: 'Raw (as received)',
      normalized: 'Normalized',
      transformed: 'Transformed',
      reply: 'Reply to sender',
      encoded: 'Encoded',
      response: 'Response',
    };
    const name = names[item.stage] ?? item.stage;
    if (item.stage === 'encoded' && item.destination) return `${name} for ${item.destination}`;
    if (item.stage === 'response' && item.destination) return `${name} from ${item.destination}`;
    return name;
  }

  $effect(() => {
    const target = info;
    if (!target) return;
    void load(target);
  });

  let ticket = 0;
  async function load(target: ContentInfo) {
    const current = ++ticket;
    loading = true;
    error = null;
    unmaskedByBreakGlass = false;
    try {
      const result = await api.content(messageId, target.stage, target.destination);
      if (current !== ticket) return;
      content = result;
      mode = segmentsFor(result.data_type, result.data ?? '') && result.encoding === 'utf8' ? 'fields' : 'text';
      if (target.stage === 'normalized') mode = 'tree';
    } catch (failure) {
      if (current !== ticket) return;
      content = null;
      error = failure;
    } finally {
      if (current === ticket) loading = false;
    }
  }

  async function unmask(event: SubmitEvent) {
    event.preventDefault();
    if (!info) return;
    if (reason.trim().length < REASON_MIN) {
      reasonError = `Give a reason of at least ${REASON_MIN} characters.`;
      return;
    }
    reasonError = null;
    try {
      content = await api.breakGlass(messageId, info.stage, info.destination, reason.trim());
      unmaskedByBreakGlass = true;
      breakGlass = false;
      reason = '';
    } catch (failure) {
      reasonError = failure instanceof ApiError ? failure.message : 'The content could not be unmasked.';
    }
  }
</script>

<section class="pane panel" aria-labelledby="{id}-heading">
  <div class="pane-header">
    <h3 id="{id}-heading" class="visually-hidden">{label}</h3>
    <div class="field grow">
      <label for="{id}-stage">{label}</label>
      <select id="{id}-stage" bind:value={selected}>
        {#each contents as item, index (index)}
          <option value={index}>{describe(item)} · {formatBytes(item.size)}</option>
        {/each}
      </select>
    </div>
    {#if content && text !== null}
      <fieldset class="modes">
        <legend class="visually-hidden">View</legend>
        <label><input type="radio" name="{id}-mode" value="text" bind:group={mode} /> Text</label>
        {#if segments}
          <label><input type="radio" name="{id}-mode" value="fields" bind:group={mode} /> Fields</label>
        {/if}
        {#if json !== undefined}
          <label><input type="radio" name="{id}-mode" value="tree" bind:group={mode} /> Tree</label>
        {/if}
      </fieldset>
    {/if}
  </div>

  <ErrorNotice {error} title="The content could not be loaded" />

  {#if loading}
    <p class="muted" role="status">Loading content…</p>
  {:else if content}
    {#if unmaskedByBreakGlass}
      <div class="notice warning" role="status">
        <p><strong>Unmasked by break-glass access.</strong> Your access and reason are recorded in the audit log.</p>
      </div>
    {:else if content.withheld}
      <div class="notice">
        <p>
          This content is withheld: its data type cannot be masked, and your role does not include
          unmasked access.
        </p>
      </div>
    {:else if content.masked}
      <div class="notice">
        <p>Patient-identifying values are masked as <span class="mono">***</span>.</p>
      </div>
    {/if}
    {#if (content.masked || content.withheld) && !unmaskedByBreakGlass && session.interactive}
      <div class="row break-glass">
        <button type="button" class="small" onclick={() => (breakGlass = true)}>Show unmasked…</button>
      </div>
    {/if}

    {#if content.data === null}
      <!-- withheld: nothing to show -->
    {:else if content.encoding === 'base64'}
      <p class="muted small-text">Binary content ({formatBytes(info?.size ?? 0)}), shown as Base64.</p>
      <!-- Scrollable regions must be reachable by keyboard (WCAG 2.1.1). -->
      <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
      <pre class="content" tabindex="0" role="region" aria-label="{label} content (Base64)">{content.data}</pre>
    {:else if mode === 'fields' && segments}
      <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
      <div class="table-wrap fields" tabindex="0" role="region" aria-label="{label} fields">
        <table>
          <thead>
            <tr><th scope="col">Field</th><th scope="col">Value</th></tr>
          </thead>
          <tbody>
            {#each segments as segment, index (index)}
              <tr class="segment-row">
                <th scope="rowgroup" colspan="2">
                  {segment.id}{segment.occurrence > 1 ? ` [${segment.occurrence}]` : ''}
                </th>
              </tr>
              {#each segment.fields as field (field.path)}
                {#if field.value !== ''}
                  <tr>
                    <td class="mono nowrap">{field.path}</td>
                    <td class="mono value">{field.value}</td>
                  </tr>
                {/if}
              {/each}
            {/each}
          </tbody>
        </table>
      </div>
    {:else if mode === 'tree' && json !== undefined}
      <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
      <div class="tree" tabindex="0" role="region" aria-label="{label} tree">
        <JsonTree value={json} />
      </div>
    {:else}
      <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
      <pre class="content" tabindex="0" role="region" aria-label="{label} content">{display}</pre>
    {/if}
  {/if}
</section>

<Dialog
  bind:open={breakGlass}
  title="Show unmasked content"
  description="Break-glass access shows patient-identifying data your role normally sees masked. Your user name, the message and your reason are recorded in the audit log."
>
  <form class="stack" onsubmit={unmask} novalidate>
    <div class="field">
      <label for="{id}-reason">Reason</label>
      <textarea
        id="{id}-reason"
        bind:value={reason}
        aria-describedby="{id}-reason-hint"
        aria-invalid={reasonError ? 'true' : undefined}
      ></textarea>
      <span class="hint" id="{id}-reason-hint">At least {REASON_MIN} characters, for example a ticket number and purpose.</span>
      {#if reasonError}<span class="error-text" role="alert">{reasonError}</span>{/if}
    </div>
    <div class="row">
      <button type="submit" class="primary">Show unmasked</button>
      <button type="button" onclick={() => (breakGlass = false)}>Cancel</button>
    </div>
  </form>
</Dialog>

<style>
  .pane {
    display: flex;
    flex-direction: column;
    gap: 0.6rem;
    min-width: 0;
  }

  .pane-header {
    display: flex;
    flex-wrap: wrap;
    align-items: flex-end;
    gap: 0.75rem;
  }

  .grow {
    flex: 1 1 14rem;
  }

  .modes {
    border: none;
    margin: 0;
    padding: 0;
    display: flex;
    gap: 0.75rem;
  }

  .modes label {
    font-weight: 500;
    display: inline-flex;
    align-items: center;
    gap: 0.25rem;
  }

  .fields,
  .tree {
    max-height: 32rem;
    overflow: auto;
  }

  .tree {
    padding: 0.5rem 0.75rem;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius);
  }

  .segment-row th {
    background: var(--surface-2);
    font-family: var(--mono);
    position: static;
  }

  .value {
    overflow-wrap: anywhere;
    white-space: pre-wrap;
  }

  .break-glass {
    margin-top: -0.25rem;
  }

  .error-text {
    color: var(--bad);
  }
</style>
