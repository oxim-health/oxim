// The live `stats` stream from `/api/v1/events` (server-sent events).
//
// The server ends each stream after a few minutes and EventSource
// reconnects on its own. When a reconnect is refused, the session is
// checked so an expired login leads back to the login page.

import { api, type StatsEvent } from './api';

export type LiveState = 'connecting' | 'live' | 'reconnecting' | 'closed';

export class LiveStats {
  latest = $state<StatsEvent | null>(null);
  state = $state<LiveState>('closed');
  /** When the last event arrived, in milliseconds since the epoch. */
  updatedAt = $state<number | null>(null);
  #source: EventSource | null = null;
  #failures = 0;

  open(): void {
    this.close();
    this.state = 'connecting';
    const source = new EventSource('/api/v1/events');
    this.#source = source;
    source.addEventListener('stats', (event) => {
      try {
        this.latest = JSON.parse((event as MessageEvent<string>).data) as StatsEvent;
        this.updatedAt = Date.now();
        this.state = 'live';
        this.#failures = 0;
      } catch {
        // Ignore a malformed event; the next one replaces it.
      }
    });
    source.addEventListener('error', () => {
      if (this.#source !== source) return;
      this.state = 'reconnecting';
      this.#failures += 1;
      if (this.#failures >= 3) {
        // Repeated failures: stop if the session ended (api.me answers 401
        // and the session store returns to the login page).
        api.me().catch(() => this.close());
      }
    });
  }

  close(): void {
    this.#source?.close();
    this.#source = null;
    this.state = 'closed';
  }
}
