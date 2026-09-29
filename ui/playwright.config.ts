import { defineConfig, devices } from '@playwright/test';

// End-to-end tests against a real `oxim run` (see e2e/harness.ts). Build
// the UI first (`npm run build`); the harness builds the oxim binary with
// cargo when target/debug/oxim is missing, or uses OXIM_BIN.
export default defineConfig({
  testDir: 'e2e',
  globalSetup: './e2e/global-setup.ts',
  globalTeardown: './e2e/global-teardown.ts',
  fullyParallel: false,
  workers: 1,
  retries: process.env.CI ? 1 : 0,
  timeout: 60_000,
  reporter: process.env.CI ? [['list'], ['html', { open: 'never' }]] : 'list',
  use: {
    trace: 'retain-on-failure',
    screenshot: 'only-on-failure',
  },
  projects: [{ name: 'chromium', use: { ...devices['Desktop Chrome'], viewport: { width: 1280, height: 900 } } }],
});
