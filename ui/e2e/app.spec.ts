import { expect, test, waitForHeading } from './fixtures';

test.describe.configure({ mode: 'serial' });

test('login page is accessible and rejects a wrong password', async ({ page, checkA11y }) => {
  await page.goto('/');
  await expect(page).toHaveURL(/\/login$/);
  await waitForHeading(page, 'Log in');
  await checkA11y('login');
  await page.getByLabel('User name').fill('admin');
  await page.getByLabel('Password').fill('not the password');
  await page.getByRole('button', { name: 'Log in' }).click();
  await expect(page.getByRole('alert')).toContainText('not correct');
});

test('the dashboard shows live channel figures', async ({ page, login, checkA11y }) => {
  await login('admin');
  await waitForHeading(page, 'Dashboard');
  const row = page.getByRole('row', { name: /lab/ });
  await expect(row).toBeVisible();
  await expect(row.getByText('Deployed')).toBeVisible();
  await expect(page.getByRole('status').getByText('Live')).toBeVisible();
  const received = page.locator('.tile', { hasText: 'Received' }).locator('.tile-value');
  await expect(received).not.toHaveText('0');
  await checkA11y('dashboard');
});

test('the message browser lists, filters and opens messages', async ({ page, login, checkA11y }) => {
  await login('admin');
  await page.getByRole('navigation', { name: 'Main' }).getByRole('link', { name: 'Messages' }).click();
  await waitForHeading(page, 'Messages');
  await expect(page.getByRole('heading', { level: 1 })).toBeFocused();
  const table = page.getByRole('table');
  await expect(table.getByRole('cell', { name: 'lab', exact: true }).first()).toBeVisible();
  await checkA11y('messages');

  await page.getByLabel('Message status').selectOption('error');
  await page.getByRole('button', { name: 'Search' }).click();
  await expect(page).toHaveURL(/status=error/);
  await expect(page.getByText('No messages match.')).toBeVisible();
  await page.getByRole('button', { name: 'Clear filters' }).click();

  await page.getByRole('table').getByRole('link').first().click();
  await waitForHeading(page, /^Message /);
  const left = page.getByRole('region', { name: 'Left view fields' });
  await expect(left).toContainText('PID-5');
  // Administrators see unmasked content.
  await expect(left).toContainText('Testpatient^Synthetic');
  await page.getByRole('radio', { name: 'Text' }).first().check();
  await expect(page.getByRole('region', { name: 'Left view content' })).toContainText('MSH|^~\\&|ANALYZER');
  await checkA11y('message detail');
});

test('viewers see masked content and can break the glass with a reason', async ({ page, login }) => {
  await login('viewer');
  await page.goto('/messages');
  await page.getByRole('table').getByRole('link').first().click();
  await waitForHeading(page, /^Message /);
  const left = page.getByRole('region', { name: 'Left view fields' });
  await expect(left).toContainText('***');
  await expect(left).not.toContainText('Testpatient');
  await expect(page.getByRole('button', { name: 'Reprocess' })).toHaveCount(0);

  const opener = page.getByRole('button', { name: 'Show unmasked…' }).first();
  await opener.click();
  const dialog = page.getByRole('dialog', { name: 'Show unmasked content' });
  await expect(dialog).toBeVisible();
  await dialog.getByRole('button', { name: 'Show unmasked' }).click();
  await expect(dialog.getByRole('alert')).toContainText('at least 8 characters');
  await dialog.getByLabel('Reason').fill('Ticket 4711: result query from ward');
  await dialog.getByRole('button', { name: 'Show unmasked' }).click();
  await expect(dialog).toBeHidden();
  await expect(page.getByText('Unmasked by break-glass access.')).toBeVisible();
  await expect(left).toContainText('Testpatient^Synthetic');

  // Viewers cannot reach administration pages.
  await page.goto('/users');
  await waitForHeading(page, 'Not permitted');
});

test('the audit log records the break-glass access', async ({ page, login, checkA11y }) => {
  await login('admin');
  await page.goto('/audit?action=message.break_glass');
  await waitForHeading(page, 'Audit log');
  const row = page.getByRole('row', { name: /message\.break_glass/ });
  await expect(row).toContainText('viewer');
  await expect(row).toContainText('Ticket 4711');
  await checkA11y('audit log');
});

test('channels can be viewed, validated and redeployed', async ({ page, login, checkA11y }) => {
  await login('admin');
  await page.goto('/channels');
  await waitForHeading(page, 'Channels');
  await checkA11y('channels');
  await page.getByRole('link', { name: 'lab', exact: true }).click();
  await waitForHeading(page, 'Channel lab');
  const structure = page.getByRole('region', { name: 'Structure' });
  await expect(structure).toContainText('mllp');
  await expect(structure).toContainText('archive');
  const editor = page.getByRole('textbox', { name: 'Channel YAML' });
  await expect(editor).toBeVisible();
  await checkA11y('channel editor');

  // An unknown source type is rejected by the server and shown inline.
  await editor.click();
  await page.keyboard.press('ControlOrMeta+a');
  await page.keyboard.insertText('id: lab\nsource:\n  type: no-such-type\n  data_type: hl7v2\n');
  await page.getByRole('button', { name: 'Save' }).click();
  await expect(page.getByRole('alert')).toContainText('no-such-type');
  await page.getByRole('button', { name: 'Revert' }).click();
  await expect(page.getByText('Unsaved changes')).toHaveCount(0);

  await page.getByRole('button', { name: 'Redeploy' }).click();
  await expect(page.getByRole('status').filter({ hasText: 'redeployed' })).toBeVisible();
});

test('code tables are edited in a grid and validated', async ({ page, login, checkA11y }) => {
  await login('admin');
  await page.goto('/tables');
  await waitForHeading(page, 'Code tables');
  await page.getByRole('link', { name: 'chemistry.csv' }).click();
  await waitForHeading(page, /chemistry\.csv/);
  await page.getByRole('button', { name: 'Add row' }).click();
  await page.getByLabel('Row 2, from').fill('GLU');
  await expect(page.getByText(/already defined in row 1/)).toBeVisible();
  await page.getByLabel('Row 2, from').fill('NA');
  await page.getByLabel('Row 2, to').fill('2951-2');
  await page.getByLabel('Row 2, display').fill('Sodium');
  await checkA11y('table editor');
  await page.getByRole('button', { name: 'Save' }).click();
  await expect(page.getByRole('status').filter({ hasText: 'Saved 2 codes' })).toBeVisible();
});

test('administrators manage users and API tokens', async ({ page, login, checkA11y }) => {
  await login('admin');
  await page.goto('/users');
  await waitForHeading(page, 'Users');
  const add = page.getByRole('button', { name: 'Add user' });
  await add.click();
  const dialog = page.getByRole('dialog', { name: 'Add user' });
  await dialog.getByLabel('User name').fill('nightshift');
  await dialog.getByLabel('Role').selectOption('operator');
  await dialog.getByLabel('Initial password').fill('night shift password');
  await checkA11y('add user dialog');
  await dialog.getByRole('button', { name: 'Add user' }).click();
  await expect(page.getByRole('status').filter({ hasText: 'nightshift was created' })).toBeVisible();
  await expect(page.getByRole('row', { name: /nightshift/ })).toBeVisible();

  // Escape closes a dialog and returns focus to the button that opened it.
  await add.click();
  await page.keyboard.press('Escape');
  await expect(add).toBeFocused();

  await page.goto('/tokens');
  await waitForHeading(page, 'API tokens');
  await page.getByRole('button', { name: 'Create token' }).click();
  const create = page.getByRole('dialog', { name: 'Create API token' });
  await create.getByLabel('Name').fill('prometheus');
  await create.getByRole('button', { name: 'Create token' }).click();
  const secret = page.getByRole('dialog', { name: /Token prometheus created/ });
  await expect(secret.getByLabel('Token')).toHaveValue(/^oxt_/);
  await secret.getByRole('button', { name: 'Done' }).click();
  await expect(page.getByRole('row', { name: /prometheus/ })).toBeVisible();
  await checkA11y('tokens');
});

test('system, account and placeholder pages are accessible', async ({ page, login, checkA11y }) => {
  await login('admin');
  for (const [path, heading] of [
    ['/system', 'System health'],
    ['/account', 'Account'],
    ['/alerts', 'Alerts'],
    ['/tables', 'Code tables'],
  ] as const) {
    await page.goto(path);
    await waitForHeading(page, heading);
    await checkA11y(path);
  }
  await expect(page.getByRole('navigation', { name: 'Main' }).getByRole('link', { name: 'Code tables' })).toHaveAttribute(
    'aria-current',
    'page',
  );
});

test('the dark theme is accessible too', async ({ page, login, checkA11y }) => {
  await page.emulateMedia({ colorScheme: 'dark' });
  await login('admin');
  await waitForHeading(page, 'Dashboard');
  await checkA11y('dashboard (dark)');
  await page.goto('/messages');
  await waitForHeading(page, 'Messages');
  await checkA11y('messages (dark)');
});

test('the skip link moves focus to the main content', async ({ page, login }) => {
  await login('admin');
  // On a fresh page load focus starts at the top of the document (after
  // in-app navigation it moves to the new page's heading instead).
  await page.reload();
  await waitForHeading(page, 'Dashboard');
  await page.keyboard.press('Tab');
  const skip = page.getByRole('link', { name: 'Skip to main content' });
  await expect(skip).toBeFocused();
  await page.keyboard.press('Enter');
  await expect(page.locator('#main')).toBeFocused();
});

test('the embedded UI is served with cache headers', async ({ request }) => {
  test.skip(process.env.OXIM_E2E_EMBEDDED !== '1', 'only when OXIM serves the UI embedded in the binary');
  const index = await request.get('/channels/lab', { headers: { Accept: 'text/html' } });
  expect(index.status()).toBe(200);
  expect(index.headers()['content-type']).toBe('text/html; charset=utf-8');
  expect(index.headers()['cache-control']).toBe('no-cache');
  const script = /src="(\/assets\/[^"]+\.js)"/.exec(await index.text())?.[1];
  expect(script).toBeTruthy();
  const asset = await request.get(script!);
  expect(asset.headers()['content-type']).toBe('text/javascript; charset=utf-8');
  expect(asset.headers()['cache-control']).toBe('public, max-age=31536000, immutable');
  expect(asset.headers()['content-security-policy']).toContain("default-src 'self'");
  expect((await request.get('/assets/missing-file.js')).status()).toBe(404);
  const api = await request.get('/api/v1/no-such-endpoint');
  expect(api.status()).toBe(404);
  expect((await api.json()).error.code).toBe('not_found');
});

test('logging out returns to the login page', async ({ page, login }) => {
  await login('admin');
  await page.getByRole('button', { name: 'Log out' }).click();
  await waitForHeading(page, 'Log in');
  await expect(page.getByRole('status').filter({ hasText: 'You have logged out.' })).toBeVisible();
  await page.goto('/channels');
  await expect(page).toHaveURL(/\/login\?next=%2Fchannels$/);
});
