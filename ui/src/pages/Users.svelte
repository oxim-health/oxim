<script lang="ts">
  import { onMount } from 'svelte';
  import Dialog from '../lib/components/Dialog.svelte';
  import ErrorNotice from '../lib/components/ErrorNotice.svelte';
  import StatusBadge from '../lib/components/StatusBadge.svelte';
  import Time from '../lib/components/Time.svelte';
  import { api, ApiError, ROLES, type Role, type User } from '../lib/api';
  import { session } from '../lib/session.svelte';

  const MIN_PASSWORD = 12;
  const ROLE_HELP: Record<Role, string> = {
    admin: 'Everything, including users, API tokens, erasure and the audit log.',
    operator: 'Day-to-day operation: unmasked messages, repairs, channel and code table changes, deployment.',
    viewer: 'Read-only access with patient-identifying values masked.',
  };

  let users = $state<User[] | null>(null);
  let error = $state<unknown>(null);
  let notice = $state<string | null>(null);

  let createOpen = $state(false);
  let draft = $state({ username: '', display_name: '', role: 'viewer' as Role, password: '' });
  let createError = $state<string | null>(null);

  let editing = $state<User | null>(null);
  let editOpen = $state(false);
  let edit = $state({ display_name: '', role: 'viewer' as Role, disabled: false });
  let editError = $state<string | null>(null);

  let passwordFor = $state<string | null>(null);
  let passwordOpen = $state(false);
  let newPassword = $state('');
  let passwordError = $state<string | null>(null);

  async function load() {
    try {
      users = (await api.users()).users;
    } catch (failure) {
      error = failure;
      users = [];
    }
  }

  onMount(load);

  function message(failure: unknown): string {
    return failure instanceof ApiError || failure instanceof Error ? failure.message : 'The change failed.';
  }

  function passwordProblem(password: string, username: string): string | null {
    if (password.length < MIN_PASSWORD) return `Use at least ${MIN_PASSWORD} characters.`;
    if (password.toLowerCase() === username.toLowerCase()) return 'The password must differ from the user name.';
    return null;
  }

  async function create(event: SubmitEvent) {
    event.preventDefault();
    const username = draft.username.trim();
    if (!username) {
      createError = 'Enter a user name.';
      return;
    }
    const problem = passwordProblem(draft.password, username);
    if (problem) {
      createError = problem;
      return;
    }
    try {
      await api.createUser({
        username,
        display_name: draft.display_name.trim() || null,
        role: draft.role,
        password: draft.password,
      });
      createOpen = false;
      notice = `User ${username} was created.`;
      draft = { username: '', display_name: '', role: 'viewer', password: '' };
      await load();
    } catch (failure) {
      createError = message(failure);
    }
  }

  function openEdit(user: User) {
    editing = user;
    edit = { display_name: user.display_name, role: user.role, disabled: user.disabled };
    editError = null;
    editOpen = true;
  }

  async function saveEdit(event: SubmitEvent) {
    event.preventDefault();
    if (!editing) return;
    try {
      await api.updateUser(editing.username, {
        display_name: edit.display_name.trim() || editing.username,
        role: edit.role,
        disabled: edit.disabled,
      });
      editOpen = false;
      notice = `User ${editing.username} was updated.`;
      await load();
    } catch (failure) {
      editError = message(failure);
    }
  }

  function openPassword(user: User) {
    passwordFor = user.username;
    newPassword = '';
    passwordError = null;
    passwordOpen = true;
  }

  async function savePassword(event: SubmitEvent) {
    event.preventDefault();
    if (!passwordFor) return;
    const problem = passwordProblem(newPassword, passwordFor);
    if (problem) {
      passwordError = problem;
      return;
    }
    try {
      await api.setPassword(passwordFor, newPassword);
      passwordOpen = false;
      notice = `The password of ${passwordFor} was set and their sessions ended.`;
    } catch (failure) {
      passwordError = message(failure);
    }
  }

  async function endSessions(user: User) {
    try {
      const result = await api.endSessions(user.username);
      notice = `${result.ended} session${result.ended === 1 ? '' : 's'} of ${user.username} ended.`;
    } catch (failure) {
      error = failure;
    }
  }
</script>

<svelte:head><title>Users · OXIM</title></svelte:head>

<div class="page-header">
  <div>
    <h1 tabindex="-1">Users</h1>
    <p>People who log in to the web UI and the API. Roles apply to every channel.</p>
  </div>
  <button
    type="button"
    class="primary"
    onclick={() => {
      createError = null;
      createOpen = true;
    }}>Add user</button
  >
</div>

<ErrorNotice {error} />
{#if notice}<div class="notice success" role="status"><p>{notice}</p></div>{/if}

{#if users === null}
  <p class="muted" role="status">Loading users…</p>
{:else}
  <div class="table-wrap">
    <table>
      <caption class="visually-hidden">Users</caption>
      <thead>
        <tr>
          <th scope="col">User</th>
          <th scope="col">Role</th>
          <th scope="col">State</th>
          <th scope="col">Last login</th>
          <th scope="col">Created</th>
          <th scope="col"><span class="visually-hidden">Actions</span></th>
        </tr>
      </thead>
      <tbody>
        {#each users as user (user.username)}
          <tr>
            <td>
              <span class="mono">{user.username}</span>
              {#if user.username === session.user?.username}<span class="badge info">You</span>{/if}
              <div class="muted small-text">{user.display_name}</div>
            </td>
            <td>{user.role}</td>
            <td><StatusBadge status={user.disabled ? 'disabled' : 'active'} /></td>
            <td class="nowrap"><Time value={user.last_login_at} /></td>
            <td class="nowrap"><Time value={user.created_at} /></td>
            <td class="actions">
              <button type="button" class="small" onclick={() => openEdit(user)} aria-label="Edit {user.username}">Edit</button>
              <button type="button" class="small" onclick={() => openPassword(user)} aria-label="Set password of {user.username}"
                >Set password</button
              >
              <button type="button" class="small" onclick={() => endSessions(user)} aria-label="End sessions of {user.username}"
                >End sessions</button
              >
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
  </div>
{/if}

<Dialog bind:open={createOpen} title="Add user">
  <form class="stack" onsubmit={create} novalidate>
    <div class="field">
      <label for="new-username">User name</label>
      <input id="new-username" bind:value={draft.username} autocomplete="off" />
    </div>
    <div class="field">
      <label for="new-display">Display name</label>
      <input id="new-display" bind:value={draft.display_name} autocomplete="off" />
      <span class="hint">Optional; the user name is used when empty.</span>
    </div>
    <div class="field">
      <label for="new-role">Role</label>
      <select id="new-role" bind:value={draft.role} aria-describedby="new-role-help">
        {#each ROLES as role (role)}<option value={role}>{role}</option>{/each}
      </select>
      <span class="hint" id="new-role-help">{ROLE_HELP[draft.role]}</span>
    </div>
    <div class="field">
      <label for="new-password">Initial password</label>
      <input id="new-password" type="password" autocomplete="new-password" bind:value={draft.password} />
      <span class="hint">At least {MIN_PASSWORD} characters. Share it securely; the user can change it under Account.</span>
    </div>
    {#if createError}<p class="error-text" role="alert">{createError}</p>{/if}
    <div class="row">
      <button type="submit" class="primary">Add user</button>
      <button type="button" onclick={() => (createOpen = false)}>Cancel</button>
    </div>
  </form>
</Dialog>

<Dialog bind:open={editOpen} title="Edit {editing?.username ?? 'user'}">
  <form class="stack" onsubmit={saveEdit} novalidate>
    <div class="field">
      <label for="edit-display">Display name</label>
      <input id="edit-display" bind:value={edit.display_name} />
    </div>
    <div class="field">
      <label for="edit-role">Role</label>
      <select id="edit-role" bind:value={edit.role} aria-describedby="edit-role-help">
        {#each ROLES as role (role)}<option value={role}>{role}</option>{/each}
      </select>
      <span class="hint" id="edit-role-help">{ROLE_HELP[edit.role]}</span>
    </div>
    <label class="checkbox">
      <input type="checkbox" bind:checked={edit.disabled} />
      Disabled (cannot log in; current sessions end)
    </label>
    {#if editError}<p class="error-text" role="alert">{editError}</p>{/if}
    <div class="row">
      <button type="submit" class="primary">Save</button>
      <button type="button" onclick={() => (editOpen = false)}>Cancel</button>
    </div>
  </form>
</Dialog>

<Dialog bind:open={passwordOpen} title="Set password of {passwordFor ?? ''}" description="The user's sessions end; they log in with the new password.">
  <form class="stack" onsubmit={savePassword} novalidate>
    <div class="field">
      <label for="reset-password">New password</label>
      <input id="reset-password" type="password" autocomplete="new-password" bind:value={newPassword} />
      <span class="hint">At least {MIN_PASSWORD} characters.</span>
    </div>
    {#if passwordError}<p class="error-text" role="alert">{passwordError}</p>{/if}
    <div class="row">
      <button type="submit" class="primary">Set password</button>
      <button type="button" onclick={() => (passwordOpen = false)}>Cancel</button>
    </div>
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

  .checkbox {
    display: inline-flex;
    align-items: center;
    gap: 0.4rem;
    font-weight: 500;
  }

  .error-text {
    color: var(--bad);
    margin: 0;
  }
</style>
