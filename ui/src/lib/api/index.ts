// The API operations the UI uses, on one shared client.

import { createClient, type Client, type Query, type Schemas } from './client';

export { ApiError, type Schemas } from './client';

export type Principal = Schemas['Principal'];
export type Permission = Schemas['Permission'];
export type Role = Schemas['Role'];
export type Channel = Schemas['Channel'];
export type BrokenChannelFile = Schemas['BrokenChannelFile'];
export type ChannelEntry = Channel | BrokenChannelFile;
export type MessageRecord = Schemas['MessageRecord'];
export type MessageDetail = Schemas['MessageDetail'];
export type ContentInfo = Schemas['ContentInfo'];
export type Content = Schemas['Content'];
export type Stage = Schemas['Stage'];
export type MessageStatus = Schemas['MessageStatus'];
export type DestinationStatus = Schemas['DestinationStatus'];
export type StatsEvent = Schemas['StatsEvent'];
export type User = Schemas['User'];
export type ApiToken = Schemas['ApiToken'];
export type AuditEvent = Schemas['AuditEvent'];
export type SystemInfo = Schemas['SystemInfo'];
export type TableInfo = Schemas['TableList']['tables'][number];

export const ROLES: readonly Role[] = ['admin', 'operator', 'viewer'];

let unauthorized: () => void = () => {};

/** Registers what happens when the session ends (a 401 answer). */
export function onUnauthorized(handler: () => void): void {
  unauthorized = handler;
}

const client: Client = createClient({ onUnauthorized: () => unauthorized() });

export function isBroken(entry: ChannelEntry): entry is BrokenChannelFile {
  return 'error' in entry;
}

export const api = {
  login: (username: string, password: string) =>
    client.request('post', '/api/v1/auth/login', { json: { username, password } }),
  logout: () => client.request('post', '/api/v1/auth/logout'),
  me: () => client.request('get', '/api/v1/auth/me'),
  changePassword: (current_password: string, new_password: string) =>
    client.request('post', '/api/v1/auth/password', { json: { current_password, new_password } }),

  channels: () => client.request('get', '/api/v1/channels'),
  channel: (id: string) => client.request('get', '/api/v1/channels/{id}', { params: { id } }),
  saveChannel: (id: string, yaml: string) =>
    client.request('put', '/api/v1/channels/{id}', {
      params: { id },
      text: yaml,
      contentType: 'application/yaml',
    }),
  deleteChannel: (id: string) => client.request('delete', '/api/v1/channels/{id}', { params: { id } }),
  deploy: (id: string) => client.request('post', '/api/v1/channels/{id}/deploy', { params: { id } }),
  undeploy: (id: string) => client.request('post', '/api/v1/channels/{id}/undeploy', { params: { id } }),
  redeploy: (id: string) => client.request('post', '/api/v1/channels/{id}/redeploy', { params: { id } }),

  messages: (query: Query) => client.request('get', '/api/v1/messages', { query }),
  message: (id: string) => client.request('get', '/api/v1/messages/{id}', { params: { id } }),
  content: (id: string, stage: Stage, destination?: string | null) =>
    client.request('get', '/api/v1/messages/{id}/content', {
      params: { id },
      query: { stage, destination },
    }),
  breakGlass: (id: string, stage: Stage, destination: string | null | undefined, reason: string) =>
    client.request('post', '/api/v1/messages/{id}/content/unmasked', {
      params: { id },
      json: { stage, destination: destination ?? null, reason },
    }),
  reprocess: (id: string) => client.request('post', '/api/v1/messages/{id}/reprocess', { params: { id } }),
  requeue: (id: string, destination: string) =>
    client.request('post', '/api/v1/messages/{id}/requeue', { params: { id }, json: { destination } }),
  erase: (id: string, reason: string) =>
    client.request('post', '/api/v1/messages/{id}/erase', { params: { id }, json: { reason } }),

  tables: () => client.request('get', '/api/v1/tables'),
  table: (name: string) => client.request('get', '/api/v1/tables/{name}', { params: { name } }),
  saveTable: (name: string, csv: string) =>
    client.request('put', '/api/v1/tables/{name}', {
      params: { name },
      text: csv,
      contentType: 'text/csv; charset=utf-8',
    }),

  users: () => client.request('get', '/api/v1/users'),
  createUser: (user: Schemas['CreateUser']) => client.request('post', '/api/v1/users', { json: user }),
  updateUser: (username: string, change: Schemas['UpdateUser']) =>
    client.request('patch', '/api/v1/users/{username}', { params: { username }, json: change }),
  setPassword: (username: string, password: string) =>
    client.request('post', '/api/v1/users/{username}/password', {
      params: { username },
      json: { password },
    }),
  endSessions: (username: string) =>
    client.request('delete', '/api/v1/users/{username}/sessions', { params: { username } }),

  tokens: () => client.request('get', '/api/v1/tokens'),
  createToken: (token: Schemas['CreateToken']) => client.request('post', '/api/v1/tokens', { json: token }),
  revokeToken: (id: number) =>
    client.request('delete', '/api/v1/tokens/{id}', { params: { id: String(id) } }),

  audit: (query: Query) => client.request('get', '/api/v1/audit', { query }),
  system: () => client.request('get', '/api/v1/system'),
};
