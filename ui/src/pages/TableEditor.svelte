<script lang="ts">
  import { onMount } from 'svelte';
  import ErrorNotice from '../lib/components/ErrorNotice.svelte';
  import { api, ApiError } from '../lib/api';
  import { parseCsv, TABLE_COLUMNS, validateTable, writeCsv } from '../lib/csv';
  import { router } from '../lib/router.svelte';
  import { session } from '../lib/session.svelte';

  let { name }: { name: string } = $props();

  const editable = session.can('edit_tables');
  let rows = $state<string[][]>([]);
  let savedText = $state('');
  let loading = $state(true);
  let error = $state<unknown>(null);
  let notice = $state<string | null>(null);
  let busy = $state(false);
  let rawMode = $state(false);
  let rawText = $state('');
  let exists = $state(true);

  let text = $derived(rawMode ? rawText : writeCsv(rows));
  let problems = $derived(validateTable(rawMode ? parseCsv(rawText) : rows));
  let dirty = $derived(text !== savedText);
  let header = $derived(rows[0] ?? []);
  let missing = $derived(TABLE_COLUMNS.filter((column) => !header.map((h) => h.trim().toLowerCase()).includes(column)));
  let badCells = $derived(new Set(problems.filter((p) => p.row > 0).map((p) => `${p.row}:${p.column}`)));

  onMount(async () => {
    try {
      const table = await api.table(name);
      rows = parseCsv(table.csv);
      savedText = writeCsv(rows);
    } catch (failure) {
      if (failure instanceof ApiError && failure.status === 404 && router.query.get('new') === '1') {
        exists = false;
        rows = [['from', 'to', 'display'], ['', '', '']];
        savedText = '';
      } else {
        error = failure;
      }
    } finally {
      loading = false;
    }
  });

  function setCell(row: number, column: number, value: string) {
    const target = rows[row];
    if (!target) return;
    while (target.length <= column) target.push('');
    target[column] = value;
  }

  function addRow() {
    rows.push(header.map(() => ''));
  }

  function removeRow(index: number) {
    rows.splice(index, 1);
  }

  function addColumn(column: string) {
    rows = rows.map((row, index) => [...row, index === 0 ? column : '']);
  }

  function toggleRaw() {
    if (rawMode) {
      rows = parseCsv(rawText);
      rawMode = false;
    } else {
      rawText = writeCsv(rows);
      rawMode = true;
    }
  }

  function columnName(index: number): string {
    return (header[index] ?? '').trim().toLowerCase();
  }

  async function save(event: SubmitEvent) {
    event.preventDefault();
    if (problems.length > 0) {
      error = new Error('Fix the problems listed below before saving.');
      return;
    }
    busy = true;
    error = null;
    notice = null;
    try {
      const result = await api.saveTable(name, text);
      savedText = text;
      exists = true;
      notice = `Saved ${result.entries} code${result.entries === 1 ? '' : 's'}. Channels use the new version when they are redeployed.`;
    } catch (failure) {
      error = failure;
    } finally {
      busy = false;
    }
  }
</script>

<svelte:head><title>{name} · Code tables · OXIM</title></svelte:head>

<div class="page-header">
  <div>
    <h1 tabindex="-1">Code table <span class="mono">{name}</span></h1>
    <p><a href="/tables">Back to code tables</a></p>
  </div>
</div>

<ErrorNotice {error} title="The table was not saved" />
{#if notice}<div class="notice success" role="status"><p>{notice}</p></div>{/if}

{#if loading}
  <p class="muted" role="status">Loading…</p>
{:else if rows.length > 0 || rawMode}
  <form class="panel stack" onsubmit={save} novalidate>
    <div class="spread">
      <p class="muted small-text help">
        Columns: <span class="mono">from</span> (required), <span class="mono">to</span> (required),
        <span class="mono">display</span>, <span class="mono">system</span> and
        <span class="mono">context</span> (a row with a context applies only there).
      </p>
      {#if editable}
        <div class="row">
          {#if dirty}<span class="badge warn">{exists ? 'Unsaved changes' : 'Not saved yet'}</span>{/if}
          <button type="button" onclick={toggleRaw}>{rawMode ? 'Edit as grid' : 'Edit as text'}</button>
          <button type="submit" class="primary" disabled={busy || !dirty}>Save</button>
        </div>
      {/if}
    </div>

    {#if rawMode}
      <div class="field">
        <label for="csv-text">CSV text</label>
        <textarea id="csv-text" class="mono csv-text" bind:value={rawText} spellcheck="false"></textarea>
      </div>
    {:else}
      <div class="table-wrap grid">
        <table>
          <caption class="visually-hidden">Codes in {name}</caption>
          <thead>
            <tr>
              <th scope="col" class="num">#</th>
              {#each header as column, index (index)}
                <th scope="col">
                  {#if editable}
                    <input
                      class="header-input mono"
                      value={column}
                      oninput={(event) => setCell(0, index, event.currentTarget.value)}
                      aria-label="Name of column {index + 1}"
                    />
                  {:else}
                    {column}
                  {/if}
                </th>
              {/each}
              {#if editable}<th scope="col"><span class="visually-hidden">Remove</span></th>{/if}
            </tr>
          </thead>
          <tbody>
            {#each rows.slice(1) as row, offset (offset)}
              {@const number = offset + 1}
              <tr>
                <td class="num muted">{number}</td>
                {#each header as _, index (index)}
                  <td>
                    {#if editable}
                      <input
                        class="cell mono"
                        value={row[index] ?? ''}
                        oninput={(event) => setCell(number, index, event.currentTarget.value)}
                        aria-label="Row {number}, {columnName(index) || `column ${index + 1}`}"
                        aria-invalid={badCells.has(`${number}:${columnName(index)}`) ? 'true' : undefined}
                      />
                    {:else}
                      <span class="mono">{row[index] ?? ''}</span>
                    {/if}
                  </td>
                {/each}
                {#if editable}
                  <td>
                    <button type="button" class="small" onclick={() => removeRow(number)} aria-label="Remove row {number}">Remove</button>
                  </td>
                {/if}
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
      {#if editable}
        <div class="row">
          <button type="button" onclick={addRow}>Add row</button>
          {#each missing as column (column)}
            <button type="button" class="small" onclick={() => addColumn(column)}>Add “{column}” column</button>
          {/each}
        </div>
      {/if}
    {/if}

    <div aria-live="polite">
      {#if problems.length > 0}
        <div class="notice warning">
          <p><strong>{problems.length} problem{problems.length === 1 ? '' : 's'}:</strong></p>
          <ul>
            {#each problems.slice(0, 20) as problem, index (index)}<li>{problem.message}</li>{/each}
          </ul>
        </div>
      {/if}
    </div>
  </form>
{/if}

<style>
  .help {
    margin: 0;
    max-width: 48rem;
  }

  .grid {
    max-height: 60vh;
  }

  .cell,
  .header-input {
    width: 100%;
    min-width: 8rem;
    padding: 0.25rem 0.4rem;
  }

  .header-input {
    font-weight: 600;
  }

  .csv-text {
    min-height: 24rem;
  }

  ul {
    margin: 0.25rem 0 0;
  }
</style>
