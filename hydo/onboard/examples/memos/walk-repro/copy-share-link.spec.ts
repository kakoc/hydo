// The walk that hydo made, schedule: run T=1:-2592000 I=fc8214f2a80db679.
// Run it against your own instance of the app, without hydo:
//   BASE_URL=http://10.89.0.11:5230 npx playwright test repro.spec.ts
// Faults on the page's own requests are page.route handlers; a fault of the backend is a
// comment at the step where hydo injected it: do the same to your instance there.
import { expect, test } from '@playwright/test';

test.use({ baseURL: process.env.BASE_URL || "http://10.89.0.11:5230" });
test.setTimeout(840000);

test("hydo walk fc8214f2a80db679", async ({ page }) => {
  const uncaught = [];
  page.on('pageerror', (e) => uncaught.push(e.message));
  page.on('console', (m) => m.type() === 'error' && console.log(`console.error: ${m.text()}`));
  // step 0
  await page.goto("/");
  await page.getByLabel("Username", { exact: true }).or(page.getByPlaceholder("Username", { exact: true })).first().fill("hydo");
  await page.getByLabel("Password", { exact: true }).or(page.getByPlaceholder("Password", { exact: true })).first().fill("hydo-pass-1");
  await page.getByRole('button', { name: "Create admin account", exact: true }).or(page.getByRole('link', { name: "Create admin account", exact: true })).first().click();
  await page.getByRole('button', { name: "Skip", exact: true }).or(page.getByRole('link', { name: "Skip", exact: true })).first().click();
  await page.getByRole("button", { name: "Welcome guide", exact: true }).nth(0).click();
  await page.waitForTimeout(700);
  await page.goBack();
  await page.waitForTimeout(700);
  await page.getByRole("textbox").nth(0).fill("0");
  await page.waitForTimeout(700);
  await page.getByRole("button", { name: "Add", exact: true }).nth(0).click();
  await page.waitForTimeout(700);
  await page.getByRole("textbox").nth(0).fill("it's");
  await page.waitForTimeout(700);
  await page.getByRole("button", { name: "Private", exact: true }).nth(0).click();
  await page.getByRole("button", { name: "Private", exact: true }).nth(0).click({ timeout: 300 }).catch(() => {});
  await page.waitForTimeout(700);
  await page.getByRole("textbox").nth(0).fill("Ünïcödé ✓ 日本語 🙂");
  await page.waitForTimeout(700);
  await page.getByRole("textbox").nth(0).fill("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
  await page.getByRole("textbox").nth(0).press('Enter');
  await page.waitForTimeout(700);
  await page.getByRole("textbox").nth(0).fill("12.50");
  await page.waitForTimeout(100);
  await page.getByRole("button", { name: "Save", exact: true }).nth(0).click();
  await page.waitForTimeout(700);
  await page.getByRole("button", { name: "More", exact: true }).nth(0).click();
  await page.waitForTimeout(700);
  await page.getByRole("link", { name: "Timeline", exact: true }).nth(0).click();
  await page.waitForTimeout(700);
  await page.getByRole("link", { name: "Calendar", exact: true }).nth(0).click();
  await page.waitForTimeout(700);
  // step 1
  // hydo: T=1:-2592000 acts from here (hydo guide faults explains the token)
  await page.goto("/calendar/2026/11?creator=hydo");
  await page.waitForTimeout(1000);
  await page.waitForTimeout(700);
  await page.getByRole("link", { name: "Attachments", exact: true }).nth(0).click();
  await page.waitForTimeout(700);
  await page.getByRole("button", { name: "New memo", exact: true }).nth(0).click();
  await page.waitForTimeout(700);
  await page.getByRole("button", { name: "Cancel", exact: true }).nth(0).click();
  await page.waitForTimeout(700);
  await page.getByRole("link", { name: "Attachments", exact: true }).nth(0).click();
  await page.waitForTimeout(700);
  await page.getByRole("link", { name: "Timeline", exact: true }).nth(0).click();
  await page.waitForTimeout(700);
  // not done in hydo's run: locator.fill: Timeout 3000ms exceeded.
  await page.waitForTimeout(700);
  await page.getByRole("button", { name: "New memo", exact: true }).nth(0).click();
  await page.waitForTimeout(700);
  // not done in hydo's run: locator.click: Timeout 3000ms exceeded.
  await page.waitForTimeout(700);
  await page.getByRole("button", { name: "Cancel", exact: true }).nth(0).click();
  await page.waitForTimeout(700);
  await page.getByRole("button", { name: "Private", exact: true }).nth(0).click();
  await page.waitForTimeout(700);
  // not done in hydo's run: locator.fill: Timeout 3000ms exceeded.
  await page.waitForTimeout(700);
  // step 2
  await page.goto("/");
  await page.reload();
  await page.waitForTimeout(700);
  await page.getByRole("textbox").nth(0).fill("Ünïcödé ✓ 日本語 🙂");
  await page.waitForTimeout(100);
  await page.getByRole("button", { name: "Save", exact: true }).nth(0).click();
  await page.waitForTimeout(700);
  // not done in hydo's run: locator.fill: Timeout 3000ms exceeded.
  await page.waitForTimeout(700);
  // not done in hydo's run: locator.fill: Timeout 3000ms exceeded.
  await page.waitForTimeout(700);
  await page.getByRole("link", { name: "now", exact: true }).nth(0).click();
  await page.waitForTimeout(700);
  await page.getByRole("button", { name: "Share", exact: true }).nth(0).click();
  await page.waitForTimeout(700);
  // not done in hydo's run: locator.fill: Timeout 3000ms exceeded.
  await page.waitForTimeout(700);
  await page.getByRole("menuitem", { name: "Manage share links", exact: true }).nth(0).click();
  await page.waitForTimeout(700);
  await page.getByRole("button", { name: "Create new link", exact: true }).nth(0).click();
  await page.getByRole("button", { name: "Create new link", exact: true }).nth(0).click({ timeout: 300 }).catch(() => {});
  await page.waitForTimeout(700);
  await page.getByRole("button", { name: "Revoke", exact: true }).nth(1).click();
  await page.waitForTimeout(700);
  await page.getByRole("button", { name: "Copy link", exact: true }).nth(0).click();
  await page.getByRole("button", { name: "Copy link", exact: true }).nth(0).click({ timeout: 300 }).catch(() => {});
  await page.waitForTimeout(700);
  await page.getByRole("combobox").nth(0).click();
  await page.waitForTimeout(700);
  // step 3
  await page.goto("/memos/b4R4VE2u3vE8gqW7sZTJtL");
  await page.getByRole("link", { name: "Calendar", exact: true }).nth(0).click();
  await page.waitForTimeout(700);
  await page.getByRole("button", { name: "Today", exact: true }).nth(0).click();
  await page.getByRole("button", { name: "Today", exact: true }).nth(0).click({ timeout: 300 }).catch(() => {});
  await page.waitForTimeout(700);
  await page.goBack();
  await page.waitForTimeout(700);
  await page.getByRole("link", { name: "12:00 AM", exact: true }).nth(0).click();
  await page.waitForTimeout(700);
  // not done in hydo's run: locator.fill: Timeout 3000ms exceeded.
  await page.waitForTimeout(700);
  await page.getByRole("button", { name: "Write a comment", exact: true }).nth(0).click();
  await page.waitForTimeout(700);
  await page.getByRole("button", { name: "Edit", exact: true }).nth(0).click();
  await page.waitForTimeout(700);
  await page.getByRole("textbox").nth(0).fill("-1");
  await page.waitForTimeout(700);
  await page.getByRole("textbox").nth(1).fill("Hello from hydo");
  await page.waitForTimeout(700);
  await page.getByRole("link", { name: "hydo", exact: true }).nth(0).click();
  await page.waitForTimeout(700);
  await page.getByRole("button", { name: "New memo", exact: true }).nth(0).click();
  await page.waitForTimeout(700);
  await page.getByRole("button", { name: "Add", exact: true }).nth(0).click();
  await page.waitForTimeout(700);
  // step 4
  await page.goto("/?creator=hydo");
  await page.reload();
  await page.waitForTimeout(700);
  await page.getByRole("button", { name: "More", exact: true }).nth(0).click();
  await page.waitForTimeout(700);
  await page.getByRole("button", { name: "Private", exact: true }).nth(0).click();
  await page.waitForTimeout(700);
  await page.getByRole("button", { name: "New memo", exact: true }).nth(0).click();
  await page.waitForTimeout(700);
  await page.waitForTimeout(1000);
  await page.waitForTimeout(700);
  await page.getByRole("textbox").nth(0).fill("Hello from hydo");
  await page.waitForTimeout(100);
  await page.getByRole("button", { name: "Save", exact: true }).nth(0).click();
  await page.waitForTimeout(700);
  await page.getByRole("textbox").nth(0).fill("12.50");
  await page.waitForTimeout(100);
  await page.getByRole("button", { name: "Save", exact: true }).nth(0).click();
  await page.waitForTimeout(700);
  await page.reload();
  await page.waitForTimeout(700);
  await page.waitForTimeout(2000);
  await page.waitForTimeout(700);
  // not done in hydo's run: locator.fill: Timeout 3000ms exceeded.
  await page.waitForTimeout(700);
  await page.getByRole("button", { name: "More", exact: true }).nth(1).click();
  await page.waitForTimeout(700);
  await page.getByRole("menuitem", { name: "Copy", exact: true }).nth(0).click();
  await page.waitForTimeout(700);
  // step 5
  await page.goto("/?creator=hydo");
  await page.getByRole("link", { name: "now", exact: true }).nth(0).click();
  await page.waitForTimeout(700);
  await page.getByRole("button", { name: "Share", exact: true }).nth(0).click();
  await page.waitForTimeout(700);
  await page.getByRole("button", { name: "Edit", exact: true }).nth(0).click();
  await page.waitForTimeout(700);
  await page.getByRole("button", { name: "2026-10-16 00:01:25", exact: true }).nth(0).click();
  await page.waitForTimeout(700);
  await page.getByRole("button", { name: "Update", exact: true }).nth(0).click();
  await page.getByRole("button", { name: "Update", exact: true }).nth(0).click({ timeout: 300 }).catch(() => {});
  await page.waitForTimeout(700);
  await page.getByRole("button", { name: "Edit", exact: true }).nth(0).click();
  await page.waitForTimeout(700);
  await page.getByRole("textbox").nth(0).fill("#tag");
  await page.getByRole("textbox").nth(0).press('Enter');
  await page.waitForTimeout(700);
  await page.getByRole("link", { name: "Attachments", exact: true }).nth(0).click();
  await page.waitForTimeout(700);
  await page.reload();
  await page.waitForTimeout(700);
  await page.getByRole("button", { name: "New memo", exact: true }).nth(0).click();
  await page.getByRole("button", { name: "New memo", exact: true }).nth(0).click({ timeout: 300 }).catch(() => {});
  await page.waitForTimeout(700);
  await page.getByRole("textbox").nth(0).fill("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
  await page.waitForTimeout(100);
  await page.getByRole("button", { name: "Save", exact: true }).nth(0).click();
  await page.waitForTimeout(700);
  await page.getByRole("link", { name: "Timeline", exact: true }).nth(0).click();
  await page.waitForTimeout(700);
  // hydo's watcher fails a run on an uncaught exception or an unhandled rejection; so does this test
  expect(uncaught, 'uncaught exceptions and unhandled rejections in the page').toEqual([]);
});
