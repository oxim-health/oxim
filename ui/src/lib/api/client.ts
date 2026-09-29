// A small typed client for the OXIM REST API.
//
// Paths, request bodies and response bodies are checked against the types
// generated from the server's OpenAPI document (schema.d.ts). Requests use
// the session cookie; changes repeat the CSRF cookie in the X-CSRF-Token
// header, as the server requires for cookie-authenticated calls.

import type { components, paths } from './schema';

export type Schemas = components['schemas'];

type Method = 'get' | 'post' | 'put' | 'patch' | 'delete';

type Operation<P extends keyof paths, M extends Method> = NonNullable<paths[P][M]>;

type JsonOf<R> = R extends { content: { 'application/json': infer B } } ? B : undefined;

/** The JSON body of an operation's success response (undefined for 204). */
export type ResponseOf<P extends keyof paths, M extends Method> =
  Operation<P, M> extends { responses: infer R }
    ? R extends { 200: infer S }
      ? JsonOf<S>
      : R extends { 201: infer S }
        ? JsonOf<S>
        : undefined
    : never;

/** The JSON request body of an operation, if it takes one. */
export type RequestOf<P extends keyof paths, M extends Method> =
  Operation<P, M> extends { requestBody: { content: { 'application/json': infer B } } }
    ? B
    : never;

type PathParams<P extends string> = P extends `${string}{${infer Name}}${infer Rest}`
  ? { [K in Name | keyof PathParams<Rest>]: string }
  : Record<never, string>;

export type Query = Record<string, string | number | undefined | null>;

export interface RequestInit<P extends keyof paths, M extends Method> {
  params?: PathParams<P & string>;
  query?: Query;
  json?: RequestOf<P, M>;
  /** A text body (channel YAML, code table CSV). */
  text?: string;
  contentType?: string;
  signal?: AbortSignal;
}

/** An error answer of the API, or a transport failure (status 0). */
export class ApiError extends Error {
  readonly status: number;
  readonly code: string;
  /** Seconds to wait before retrying, for `429` answers. */
  readonly retryAfter: number | undefined;

  constructor(status: number, code: string, message: string, retryAfter?: number) {
    super(message);
    this.name = 'ApiError';
    this.status = status;
    this.code = code;
    this.retryAfter = retryAfter;
  }
}

export const CSRF_COOKIE = 'oxim_csrf';
export const CSRF_HEADER = 'X-CSRF-Token';

/** The value of a cookie in a `document.cookie` string. */
export function readCookie(cookies: string, name: string): string | undefined {
  for (const part of cookies.split(';')) {
    const index = part.indexOf('=');
    if (index < 0) continue;
    if (part.slice(0, index).trim() === name) {
      return decodeURIComponent(part.slice(index + 1).trim());
    }
  }
  return undefined;
}

/** Fills `{name}` placeholders of an OpenAPI path template. */
export function fillPath(template: string, params: Record<string, string> = {}): string {
  return template.replace(/\{(\w+)\}/g, (_, name: string) => {
    const value = params[name];
    if (value === undefined) throw new Error(`missing path parameter ${name}`);
    return encodeURIComponent(value);
  });
}

/** Appends the defined, non-empty query parameters to a path. */
export function withQuery(path: string, query: Query = {}): string {
  const search = new URLSearchParams();
  for (const [key, value] of Object.entries(query)) {
    if (value !== undefined && value !== null && value !== '') search.set(key, String(value));
  }
  const text = search.toString();
  return text ? `${path}?${text}` : path;
}

export interface ClientOptions {
  fetch?: typeof fetch;
  /** Reads the document cookies (for the CSRF token). */
  cookies?: () => string;
  /** Called when the server answers 401: the session ended. */
  onUnauthorized?: () => void;
}

async function errorFrom(response: Response): Promise<ApiError> {
  let code = 'http_error';
  let message = `${response.status} ${response.statusText}`.trim();
  try {
    const body: unknown = await response.json();
    const error = (body as { error?: { code?: unknown; message?: unknown } }).error;
    if (error && typeof error.code === 'string') code = error.code;
    if (error && typeof error.message === 'string') message = error.message;
  } catch {
    // Not a JSON error body: keep the status line.
  }
  const retry = Number(response.headers.get('retry-after'));
  return new ApiError(response.status, code, message, Number.isFinite(retry) && retry > 0 ? retry : undefined);
}

export interface Client {
  request<P extends keyof paths, M extends Method>(
    method: M,
    path: P,
    init?: RequestInit<P, M>,
  ): Promise<ResponseOf<P, M>>;
}

export function createClient(options: ClientOptions = {}): Client {
  const doFetch = options.fetch ?? ((input, init) => fetch(input, init));
  const cookies = options.cookies ?? (() => (typeof document === 'undefined' ? '' : document.cookie));
  return {
    async request(method, path, init = {}) {
      const url = withQuery(fillPath(path as string, init.params as Record<string, string>), init.query);
      const headers: Record<string, string> = { Accept: 'application/json' };
      let body: string | undefined;
      if (init.json !== undefined) {
        headers['Content-Type'] = 'application/json';
        body = JSON.stringify(init.json);
      } else if (init.text !== undefined) {
        headers['Content-Type'] = init.contentType ?? 'text/plain; charset=utf-8';
        body = init.text;
      }
      if (method !== 'get') {
        const csrf = readCookie(cookies(), CSRF_COOKIE);
        if (csrf) headers[CSRF_HEADER] = csrf;
      }
      let response: Response;
      try {
        response = await doFetch(url, {
          method: method.toUpperCase(),
          headers,
          body,
          credentials: 'same-origin',
          signal: init.signal,
        });
      } catch (error) {
        if (error instanceof DOMException && error.name === 'AbortError') throw error;
        throw new ApiError(0, 'network', 'The server cannot be reached.');
      }
      if (!response.ok) {
        const error = await errorFrom(response);
        if (response.status === 401 && path !== '/api/v1/auth/login') options.onUnauthorized?.();
        throw error;
      }
      if (response.status === 204) return undefined as never;
      const text = await response.text();
      return (text ? JSON.parse(text) : undefined) as never;
    },
  };
}
