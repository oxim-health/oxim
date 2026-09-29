<script lang="ts">
  import { statusLabel } from '../format';

  let { status }: { status: string } = $props();

  const TONES: Record<string, string> = {
    completed: 'ok',
    active: 'ok',
    sent: 'ok',
    deployed: 'ok',
    live: 'ok',
    transformed: 'info',
    received: 'info',
    queued: 'info',
    sending: 'info',
    connecting: 'info',
    retrying: 'warn',
    reconnecting: 'warn',
    filtered: 'neutral',
    stopped: 'neutral',
    disabled: 'neutral',
    revoked: 'neutral',
    closed: 'neutral',
    error: 'bad',
    failed: 'bad',
    invalid: 'bad',
  };

  let tone = $derived(TONES[status] ?? 'neutral');
</script>

<span class="badge {tone}">{statusLabel(status)}</span>
