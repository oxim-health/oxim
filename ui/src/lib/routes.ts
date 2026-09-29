// Client-side routes: matching, titles and the permission each page needs.
// Pure functions, so they are unit-tested without a browser.

import type { Permission } from './api';

export type RouteName =
  | 'dashboard'
  | 'login'
  | 'channels'
  | 'channel-new'
  | 'channel'
  | 'messages'
  | 'message'
  | 'tables'
  | 'table'
  | 'users'
  | 'tokens'
  | 'audit'
  | 'system'
  | 'account'
  | 'alerts'
  | 'devices'
  | 'backups'
  | 'not-found';

export interface Route {
  name: RouteName;
  params: Record<string, string>;
}

const PATTERNS: [string, RouteName][] = [
  ['/', 'dashboard'],
  ['/login', 'login'],
  ['/channels', 'channels'],
  ['/channels/new', 'channel-new'],
  ['/channels/:id', 'channel'],
  ['/messages', 'messages'],
  ['/messages/:id', 'message'],
  ['/tables', 'tables'],
  ['/tables/:name', 'table'],
  ['/users', 'users'],
  ['/tokens', 'tokens'],
  ['/audit', 'audit'],
  ['/system', 'system'],
  ['/account', 'account'],
  ['/alerts', 'alerts'],
  ['/devices', 'devices'],
  ['/backups', 'backups'],
];

/** The route for a location path. */
export function matchRoute(pathname: string): Route {
  const path = pathname.length > 1 ? pathname.replace(/\/+$/, '') : pathname;
  const parts = path.split('/').filter(Boolean);
  for (const [pattern, name] of PATTERNS) {
    const expected = pattern.split('/').filter(Boolean);
    if (expected.length !== parts.length) continue;
    const params: Record<string, string> = {};
    let matched = true;
    for (let index = 0; index < expected.length; index++) {
      const want = expected[index] as string;
      const have = parts[index] as string;
      if (want.startsWith(':')) {
        try {
          params[want.slice(1)] = decodeURIComponent(have);
        } catch {
          matched = false;
          break;
        }
      } else if (want !== have) {
        matched = false;
        break;
      }
    }
    if (matched) return { name, params };
  }
  return { name: 'not-found', params: {} };
}

/** The permission a page needs; `null` for any logged-in user. */
export const PAGE_PERMISSION: Record<RouteName, Permission | null> = {
  dashboard: 'view_dashboard',
  login: null,
  channels: 'view_channels',
  'channel-new': 'edit_channels',
  channel: 'view_channels',
  messages: 'view_messages',
  message: 'view_messages',
  tables: 'view_tables',
  table: 'view_tables',
  users: 'manage_users',
  tokens: 'manage_tokens',
  audit: 'view_audit',
  system: 'view_system',
  account: null,
  alerts: 'view_dashboard',
  devices: 'view_dashboard',
  backups: 'view_system',
  'not-found': null,
};

export interface NavItem {
  href: string;
  label: string;
  permission: Permission | null;
  section: 'operate' | 'configure' | 'administer';
}

export const NAV: NavItem[] = [
  { href: '/', label: 'Dashboard', permission: 'view_dashboard', section: 'operate' },
  { href: '/messages', label: 'Messages', permission: 'view_messages', section: 'operate' },
  { href: '/alerts', label: 'Alerts', permission: 'view_dashboard', section: 'operate' },
  { href: '/devices', label: 'Devices', permission: 'view_dashboard', section: 'operate' },
  { href: '/channels', label: 'Channels', permission: 'view_channels', section: 'configure' },
  { href: '/tables', label: 'Code tables', permission: 'view_tables', section: 'configure' },
  { href: '/users', label: 'Users', permission: 'manage_users', section: 'administer' },
  { href: '/tokens', label: 'API tokens', permission: 'manage_tokens', section: 'administer' },
  { href: '/audit', label: 'Audit log', permission: 'view_audit', section: 'administer' },
  { href: '/system', label: 'System health', permission: 'view_system', section: 'administer' },
  { href: '/backups', label: 'Backups', permission: 'view_system', section: 'administer' },
];

/** Whether a nav item is the current page (or a parent of it). */
export function isCurrent(href: string, pathname: string): boolean {
  if (href === '/') return pathname === '/';
  return pathname === href || pathname.startsWith(`${href}/`);
}

/** Whether a link target is handled by the client-side router. */
export function isClientPath(href: string): boolean {
  return (
    href.startsWith('/') &&
    !href.startsWith('//') &&
    !href.startsWith('/api/') &&
    href !== '/metrics' &&
    !href.startsWith('/metrics?')
  );
}
