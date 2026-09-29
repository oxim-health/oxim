// The logged-in user and what they may do.

import { api, onUnauthorized, type Permission, type Principal } from './api';

export type SessionState = 'unknown' | 'anonymous' | 'authenticated';

class Session {
  user = $state<Principal | null>(null);
  state = $state<SessionState>('unknown');
  /** Shown on the login page, for example after the session expired. */
  notice = $state<string | null>(null);

  constructor() {
    onUnauthorized(() => this.expire('Your session has ended. Log in again to continue.'));
  }

  can(permission: Permission | null): boolean {
    if (permission === null) return this.user !== null;
    return this.user?.permissions.includes(permission) ?? false;
  }

  /** Whether the user logged in interactively (break-glass and password
   * changes are only for sessions, not API tokens). */
  get interactive(): boolean {
    return this.user?.kind === 'session';
  }

  async restore(): Promise<void> {
    try {
      this.user = await api.me();
      this.state = 'authenticated';
    } catch {
      this.user = null;
      this.state = 'anonymous';
    }
  }

  async login(username: string, password: string): Promise<void> {
    const response = await api.login(username, password);
    this.user = response.user;
    this.notice = null;
    this.state = 'authenticated';
  }

  async logout(): Promise<void> {
    try {
      await api.logout();
    } finally {
      this.user = null;
      this.state = 'anonymous';
      this.notice = 'You have logged out.';
    }
  }

  expire(notice: string): void {
    if (this.state !== 'authenticated') return;
    this.user = null;
    this.state = 'anonymous';
    this.notice = notice;
  }
}

export const session = new Session();
