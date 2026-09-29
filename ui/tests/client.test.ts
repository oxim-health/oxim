import { describe, expect, it, vi } from 'vitest';
import { ApiError, createClient, fillPath, readCookie, withQuery } from '../src/lib/api/client';

function reply(status: number, body?: unknown, headers: Record<string, string> = {}): Response {
  return new Response(body === undefined ? null : JSON.stringify(body), {
    status,
    headers: { 'content-type': 'application/json', ...headers },
  });
}

describe('request helpers', () => {
  it('reads cookies', () => {
    expect(readCookie('a=1; oxim_csrf=abc%3D; b=2', 'oxim_csrf')).toBe('abc=');
    expect(readCookie('a=1', 'oxim_csrf')).toBeUndefined();
    expect(readCookie('', 'oxim_csrf')).toBeUndefined();
  });

  it('fills path templates and encodes parameters', () => {
    expect(fillPath('/api/v1/channels/{id}/deploy', { id: 'lab 1' })).toBe('/api/v1/channels/lab%201/deploy');
    expect(() => fillPath('/api/v1/users/{username}', {})).toThrow('missing path parameter username');
  });

  it('adds only defined, non-empty query parameters', () => {
    expect(withQuery('/api/v1/messages', { channel: 'lab', status: '', before: undefined, limit: 50 })).toBe(
      '/api/v1/messages?channel=lab&limit=50',
    );
    expect(withQuery('/x', {})).toBe('/x');
  });
});

describe('client', () => {
  it('sends JSON with the CSRF header on changes, not on reads', async () => {
    const fetch = vi.fn(async (_url: RequestInfo | URL, _init?: RequestInit) => reply(200, { ok: true }));
    const client = createClient({ fetch, cookies: () => 'oxim_csrf=token-1' });

    await client.request('get', '/api/v1/auth/me');
    const [, readInit] = fetch.mock.calls[0]!;
    expect((readInit?.headers as Record<string, string>)['X-CSRF-Token']).toBeUndefined();
    expect(readInit?.credentials).toBe('same-origin');

    await client.request('post', '/api/v1/messages/{id}/erase', {
      params: { id: '01J0000000000000000000000' },
      json: { reason: 'synthetic test' },
    });
    const [url, init] = fetch.mock.calls[1]!;
    expect(url).toBe('/api/v1/messages/01J0000000000000000000000/erase');
    expect(init?.method).toBe('POST');
    const headers = init?.headers as Record<string, string>;
    expect(headers['X-CSRF-Token']).toBe('token-1');
    expect(headers['Content-Type']).toBe('application/json');
    expect(init?.body).toBe('{"reason":"synthetic test"}');
  });

  it('sends text bodies with their content type', async () => {
    const fetch = vi.fn(async () => reply(200, { id: 'lab', file: 'lab.yaml' }));
    const client = createClient({ fetch, cookies: () => '' });
    const saved = await client.request('put', '/api/v1/channels/{id}', {
      params: { id: 'lab' },
      text: 'id: lab\n',
      contentType: 'application/yaml',
    });
    expect(saved.file).toBe('lab.yaml');
    const [, init] = fetch.mock.calls[0] as unknown as [string, RequestInit];
    expect((init.headers as Record<string, string>)['Content-Type']).toBe('application/yaml');
    expect(init.body).toBe('id: lab\n');
  });

  it('returns undefined for 204 answers', async () => {
    const client = createClient({ fetch: async () => new Response(null, { status: 204 }), cookies: () => '' });
    await expect(client.request('post', '/api/v1/channels/{id}/deploy', { params: { id: 'lab' } })).resolves.toBe(
      undefined,
    );
  });

  it('turns error bodies into ApiError and reports ended sessions', async () => {
    const onUnauthorized = vi.fn();
    const client = createClient({
      fetch: async () => reply(401, { error: { code: 'unauthorized', message: 'log in first' } }),
      cookies: () => '',
      onUnauthorized,
    });
    const failure = await client.request('get', '/api/v1/channels').catch((error: unknown) => error);
    expect(failure).toBeInstanceOf(ApiError);
    expect((failure as ApiError).status).toBe(401);
    expect((failure as ApiError).code).toBe('unauthorized');
    expect((failure as ApiError).message).toBe('log in first');
    expect(onUnauthorized).toHaveBeenCalledOnce();
  });

  it('does not treat a failed login as an ended session', async () => {
    const onUnauthorized = vi.fn();
    const client = createClient({
      fetch: async () => reply(401, { error: { code: 'invalid_credentials', message: 'wrong' } }),
      cookies: () => '',
      onUnauthorized,
    });
    await expect(
      client.request('post', '/api/v1/auth/login', { json: { username: 'a', password: 'b' } }),
    ).rejects.toBeInstanceOf(ApiError);
    expect(onUnauthorized).not.toHaveBeenCalled();
  });

  it('keeps Retry-After of throttled logins', async () => {
    const client = createClient({
      fetch: async () =>
        reply(429, { error: { code: 'too_many_attempts', message: 'later' } }, { 'retry-after': '120' }),
      cookies: () => '',
    });
    const failure = (await client
      .request('post', '/api/v1/auth/login', { json: { username: 'a', password: 'b' } })
      .catch((error: unknown) => error)) as ApiError;
    expect(failure.retryAfter).toBe(120);
  });

  it('reports network failures as status 0', async () => {
    const client = createClient({
      fetch: async () => {
        throw new TypeError('connection refused');
      },
      cookies: () => '',
    });
    const failure = (await client.request('get', '/api/v1/system').catch((error: unknown) => error)) as ApiError;
    expect(failure.status).toBe(0);
    expect(failure.code).toBe('network');
  });

  it('falls back to the status line for non-JSON errors', async () => {
    const client = createClient({
      fetch: async () => new Response('gateway down', { status: 502, statusText: 'Bad Gateway' }),
      cookies: () => '',
    });
    const failure = (await client.request('get', '/api/v1/system').catch((error: unknown) => error)) as ApiError;
    expect(failure.message).toBe('502 Bad Gateway');
    expect(failure.code).toBe('http_error');
  });
});
