import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

function reply(status: number, body?: unknown): Response {
  return new Response(body === undefined ? null : JSON.stringify(body), {
    status,
    headers: { 'content-type': 'application/json' },
  });
}

const ADMIN = {
  username: 'admin',
  display_name: 'Administrator',
  role: 'admin',
  kind: 'session',
  permissions: ['view_dashboard', 'view_messages', 'manage_users'],
};

describe('session store', () => {
  beforeEach(() => {
    vi.resetModules();
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it('restores, checks permissions, expires on 401 and logs out', async () => {
    const fetch = vi.fn(async (url: string) => (url === '/api/v1/auth/me' ? reply(200, ADMIN) : reply(204)));
    vi.stubGlobal('fetch', fetch);
    const { session } = await import('../src/lib/session.svelte');
    expect(session.state).toBe('unknown');
    await session.restore();
    expect(session.state).toBe('authenticated');
    expect(session.can('manage_users')).toBe(true);
    expect(session.can('erase_messages')).toBe(false);
    expect(session.can(null)).toBe(true);
    expect(session.interactive).toBe(true);

    // Any 401 ends the session with a notice for the login page.
    fetch.mockImplementation(async () => reply(401, { error: { code: 'unauthorized', message: 'expired' } }));
    const { api } = await import('../src/lib/api');
    await expect(api.channels()).rejects.toThrow('expired');
    expect(session.state).toBe('anonymous');
    expect(session.user).toBeNull();
    expect(session.notice).toMatch(/session has ended/);
  });

  it('logs in and out', async () => {
    const fetch = vi.fn(async (url: string) => {
      if (url === '/api/v1/auth/login') {
        return reply(200, { token: 't', csrf_token: 'c', expires_at: '2026-09-30T00:00:00Z', user: ADMIN });
      }
      if (url === '/api/v1/auth/me') return reply(401, { error: { code: 'unauthorized', message: 'no' } });
      return reply(204);
    });
    vi.stubGlobal('fetch', fetch);
    const { session } = await import('../src/lib/session.svelte');
    await session.restore();
    expect(session.state).toBe('anonymous');
    await session.login('admin', 'correct horse battery');
    expect(session.state).toBe('authenticated');
    expect(session.user?.display_name).toBe('Administrator');
    expect(session.notice).toBeNull();
    await session.logout();
    expect(session.state).toBe('anonymous');
    expect(session.notice).toBe('You have logged out.');
  });
});

class FakeEventSource {
  static last: FakeEventSource | null = null;
  listeners = new Map<string, ((event: MessageEvent<string>) => void)[]>();
  closed = false;

  constructor(readonly url: string) {
    FakeEventSource.last = this;
  }

  addEventListener(type: string, listener: (event: MessageEvent<string>) => void) {
    this.listeners.set(type, [...(this.listeners.get(type) ?? []), listener]);
  }

  emit(type: string, data = '') {
    for (const listener of this.listeners.get(type) ?? []) listener({ data } as MessageEvent<string>);
  }

  close() {
    this.closed = true;
  }
}

describe('live stats', () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it('tracks the stream state and the latest event', async () => {
    vi.stubGlobal('EventSource', FakeEventSource);
    vi.stubGlobal('fetch', vi.fn(async () => reply(200, ADMIN)));
    const { LiveStats } = await import('../src/lib/live.svelte');
    const live = new LiveStats();
    live.open();
    const source = FakeEventSource.last!;
    expect(source.url).toBe('/api/v1/events');
    expect(live.state).toBe('connecting');

    source.emit('stats', JSON.stringify({ deployed: ['lab'], messages: [], deliveries: [] }));
    expect(live.state).toBe('live');
    expect(live.latest?.deployed).toEqual(['lab']);
    expect(live.updatedAt).toBeTypeOf('number');

    source.emit('stats', 'not json');
    expect(live.latest?.deployed).toEqual(['lab']);

    source.emit('error');
    expect(live.state).toBe('reconnecting');
    live.close();
    expect(source.closed).toBe(true);
    expect(live.state).toBe('closed');
  });
});
