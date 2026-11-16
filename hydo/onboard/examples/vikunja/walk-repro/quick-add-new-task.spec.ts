// Finding 2 of RESULTS.md as a Playwright test, without hydo: the quick action bar's "New task"
// while the server fails to create the task. hydo's walk met it under disk I/O errors of the
// server; here page.route answers the create with the 500 that Vikunja then gives.
//   BASE_URL=http://<your Vikunja>:3456 VIKUNJA_USER=... VIKUNJA_PASS=... npx playwright test quick-add-new-task.spec.ts
// It fails on Vikunja 2.8.0: the page leaves the rejection unhandled, and the typed title is gone.
import { expect, test } from '@playwright/test';

test.use({ baseURL: process.env.BASE_URL || 'http://10.89.0.11:3456' });

test('a quick-add task that the server refuses keeps its title and throws nothing', async ({ page }) => {
  const uncaught: string[] = [];
  page.on('pageerror', (e) => uncaught.push(e.message));
  await page.goto('/login');
  await page.getByRole('textbox', { name: 'Username Or Email Address' }).fill(process.env.VIKUNJA_USER || 'hydo');
  await page.getByRole('textbox', { name: 'Password', exact: true }).fill(process.env.VIKUNJA_PASS || 'hydo-pass-1');
  await page.getByRole('button', { name: 'Login', exact: true }).click();
  await page.getByText('Current Tasks').waitFor();
  // the create fails as it does under the server's disk errors
  await page.route(/\/api\/v2\/projects\/\d+\/tasks$/, (route) =>
    route.request().method() !== 'POST'
      ? route.fallback()
      : route.fulfill({
          status: 500,
          contentType: 'application/problem+json',
          body: JSON.stringify({ status: 500, title: 'Internal Server Error', detail: 'unexpected error occurred' }),
        }),
  );
  await page.getByRole('button', { name: 'Open the search/quick action bar' }).click();
  await page.getByRole('button', { name: 'New task Command' }).click();
  const title = page.getByRole('textbox', { name: 'Enter the title of the new task…' });
  await title.fill('call the plumber');
  await title.press('Enter');
  await page.waitForTimeout(3000);
  await expect(page.locator('.vue-notification')).toContainText('unexpected error occurred');
  expect(uncaught, 'uncaught exceptions and unhandled rejections in the page').toEqual([]);
  await expect(title, 'the title the user typed').toHaveValue('call the plumber');
});
