<script lang="ts">
  // A modal dialog on the native <dialog> element: the browser traps focus
  // inside it and closes it with Escape. Focus returns to the element that
  // opened it.
  import type { Snippet } from 'svelte';

  let {
    open = $bindable(false),
    title,
    description,
    children,
    onclose,
    wide = false,
  }: {
    open?: boolean;
    title: string;
    description?: string;
    children: Snippet;
    onclose?: () => void;
    wide?: boolean;
  } = $props();

  const id = $props.id();
  let dialog: HTMLDialogElement | undefined = $state();
  let opener: HTMLElement | null = null;

  $effect(() => {
    if (!dialog) return;
    if (open && !dialog.open) {
      opener = document.activeElement instanceof HTMLElement ? document.activeElement : null;
      dialog.showModal();
    } else if (!open && dialog.open) {
      dialog.close();
    }
  });

  function closed() {
    open = false;
    onclose?.();
    opener?.focus();
    opener = null;
  }
</script>

<dialog
  bind:this={dialog}
  class:wide
  aria-labelledby="{id}-title"
  aria-describedby={description ? `${id}-description` : undefined}
  onclose={closed}
>
  <div class="dialog-body">
    <h2 id="{id}-title">{title}</h2>
    {#if description}
      <p id="{id}-description" class="muted">{description}</p>
    {/if}
    {@render children()}
  </div>
</dialog>

<style>
  dialog {
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--surface);
    color: var(--text);
    padding: 0;
    width: min(32rem, calc(100vw - 2rem));
    box-shadow: 0 10px 40px rgb(0 0 0 / 30%);
  }

  dialog.wide {
    width: min(48rem, calc(100vw - 2rem));
  }

  dialog::backdrop {
    background: rgb(10 15 20 / 55%);
  }

  .dialog-body {
    padding: 1.1rem 1.25rem 1.25rem;
    display: flex;
    flex-direction: column;
    gap: 0.75rem;
  }

  h2 {
    margin: 0;
  }

  p {
    margin: 0;
  }
</style>
