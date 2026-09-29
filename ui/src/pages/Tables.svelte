<script lang="ts">
  import { onMount } from 'svelte';
  import ErrorNotice from '../lib/components/ErrorNotice.svelte';
  import { api, type TableInfo } from '../lib/api';
  import { formatBytes, formatCount } from '../lib/format';
  import { router } from '../lib/router.svelte';
  import { session } from '../lib/session.svelte';

  let tables = $state<TableInfo[] | null>(null);
  let error = $state<unknown>(null);
  let newName = $state('');
  let nameError = $state<string | null>(null);

  onMount(async () => {
    try {
      tables = (await api.tables()).tables;
    } catch (failure) {
      error = failure;
      tables = [];
    }
  });

  function create(event: SubmitEvent) {
    event.preventDefault();
    let name = newName.trim();
    if (name && !name.endsWith('.csv')) name = `${name}.csv`;
    if (!/^[A-Za-z0-9][A-Za-z0-9._-]{0,123}\.csv$/.test(name)) {
      nameError = 'Use letters, digits, ".", "_" and "-", ending in .csv.';
      return;
    }
    if (tables?.some((table) => table.name === name)) {
      nameError = `${name} already exists.`;
      return;
    }
    nameError = null;
    router.navigate(`/tables/${encodeURIComponent(name)}?new=1`);
  }
</script>

<svelte:head><title>Code tables · OXIM</title></svelte:head>

<div class="page-header">
  <div>
    <h1 tabindex="-1">Code tables</h1>
    <p>CSV tables that translate codes, for example an analyzer's test codes to LIS or LOINC codes.</p>
  </div>
</div>

<ErrorNotice {error} />

{#if session.can('edit_tables')}
  <form class="panel new-table" onsubmit={create} novalidate>
    <div class="field">
      <label for="new-table">New table</label>
      <div class="row">
        <input
          id="new-table"
          bind:value={newName}
          placeholder="chemistry.csv"
          aria-describedby="new-table-hint"
          aria-invalid={nameError ? 'true' : undefined}
        />
        <button type="submit">Create</button>
      </div>
      <span class="hint" id="new-table-hint">A file name in the tables directory, ending in .csv.</span>
      {#if nameError}<span class="error-text" role="alert">{nameError}</span>{/if}
    </div>
  </form>
{/if}

{#if tables === null}
  <p class="muted" role="status">Loading tables…</p>
{:else if tables.length === 0}
  <p class="muted">No code tables yet.</p>
{:else}
  <div class="table-wrap">
    <table>
      <caption class="visually-hidden">Code tables</caption>
      <thead>
        <tr>
          <th scope="col">Table</th>
          <th scope="col" class="num">Codes</th>
          <th scope="col" class="num">Size</th>
          <th scope="col">Problems</th>
        </tr>
      </thead>
      <tbody>
        {#each tables as table (table.name)}
          <tr>
            <td><a class="mono" href="/tables/{encodeURIComponent(table.name)}">{table.name}</a></td>
            <td class="num">{table.entries === null ? '—' : formatCount(table.entries)}</td>
            <td class="num">{formatBytes(table.size)}</td>
            <td>
              {#if table.error}<span class="badge bad">Invalid</span> <span class="small-text">{table.error}</span>{:else}—{/if}
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
  </div>
{/if}

<style>
  .new-table {
    margin-bottom: 1rem;
    max-width: 32rem;
  }

  .error-text {
    color: var(--bad);
    font-size: 0.85rem;
  }
</style>
