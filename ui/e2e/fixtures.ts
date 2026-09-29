// Shared fixtures: the running OXIM, logging in, accessibility checks and a
// guard that fails a test on any Content-Security-Policy violation.

import AxeBuilder from '@axe-core/playwright';
import { test as base, expect, type Page } from '@playwright/test';
import { readState, type E2eState } from './harness';

interface Fixtures {
  oxim: E2eState;
  cspViolations: string[];
  login: (username: 'admin' | 'viewer') => Promise<void>;
  checkA11y: (label: string) => Promise<void>;
}

export const test = base.extend<Fixtures>({
  // eslint-disable-next-line no-empty-pattern
  oxim: async ({}, use) => {
    await use(readState());
  },
  baseURL: async ({ oxim }, use) => {
    await use(oxim.baseURL);
  },
  cspViolations: [
    async ({ page }, use) => {
      const violations: string[] = [];
      page.on('console', (message) => {
        const text = message.text();
        if (/Content Security Policy|Refused to (apply|load|execute)/i.test(text)) {
          const where = message.location();
          violations.push(`${text} [at ${where.url}:${where.lineNumber}:${where.columnNumber}]`);
        }
      });
      page.on('pageerror', (error) => violations.push(`page error: ${error.message}`));
      await use(violations);
      expect(violations, 'no CSP violations or page errors').toEqual([]);
    },
    { auto: true },
  ],
  login: async ({ page, oxim }, use) => {
    await use(async (username) => {
      await page.goto('/login');
      await page.getByLabel('User name').fill(username);
      await page.getByLabel('Password').fill(username === 'admin' ? oxim.adminPassword : oxim.viewerPassword);
      await page.getByRole('button', { name: 'Log in' }).click();
      await expect(page.getByRole('button', { name: 'Log out' })).toBeVisible();
    });
  },
  checkA11y: async ({ page }, use) => {
    await use(async (label) => {
      const results = await new AxeBuilder({ page })
        .withTags(['wcag2a', 'wcag2aa', 'wcag21a', 'wcag21aa'])
        .analyze();
      const summary = results.violations.map(
        (violation) =>
          `${violation.id} (${violation.impact}): ${violation.help}\n  ${violation.nodes
            .slice(0, 3)
            .map((node) => node.target.join(' '))
            .join('\n  ')}`,
      );
      expect(summary, `accessibility violations on ${label}`).toEqual([]);
    });
  },
});

export { expect };

export async function waitForHeading(page: Page, name: string | RegExp): Promise<void> {
  await expect(page.getByRole('heading', { level: 1, name })).toBeVisible();
}
