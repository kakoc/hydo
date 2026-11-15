// The walk that hydo made, schedule: run H=api.get_order@3-4:reset I=a0d28305209409f6,n2,x0.0,x1.0,x2.0.
// Run it against your own instance of the app, without hydo:
//   BASE_URL=http://localhost:3000 npx playwright test repro.spec.ts
// Faults on the page's own requests are page.route handlers; a fault of the backend is a
// comment at the step where hydo injected it: do the same to your instance there.
import { expect, test } from '@playwright/test';

test.use({ baseURL: process.env.BASE_URL || "http://localhost:3000" });
test.setTimeout(600000);

test("hydo walk a0d28305209409f6,n2,x0.0,x1.0,x2.0", async ({ page }) => {
  const uncaught = [];
  page.on('pageerror', (e) => uncaught.push(e.message));
  page.on('console', (m) => m.type() === 'error' && console.log(`console.error: ${m.text()}`));
  // step 0
  await page.goto("/");
  await page.getByRole("button", { name: "Add Coffee mug to cart", exact: true }).nth(0).click();
  await page.waitForTimeout(700);
  // step 1
  await page.goto("/");
  await page.getByRole("link", { name: "Shop", exact: true }).nth(0).click();
  await page.waitForTimeout(700);
  // step 2
  await page.goto("/");
  await page.getByRole("button", { name: "Add Tea pot to cart", exact: true }).nth(0).click();
  await page.waitForTimeout(700);
  // step 3
  await page.route("**/api/orders/*", async (route) => { if (route.request().method() !== "GET") return route.fallback(); return (route.abort('connectionreset')); }); // H=api.get_order@3-4:reset
  await page.goto("/");
  await page.getByRole("link", { name: "Cart (2)", exact: true }).nth(0).click();
  await page.waitForTimeout(700);
  await page.getByRole("button", { name: "Place order", exact: true }).nth(0).click();
  await page.waitForTimeout(700);
  // hydo's watcher fails a run on an uncaught exception or an unhandled rejection; so does this test
  expect(uncaught, 'uncaught exceptions and unhandled rejections in the page').toEqual([]);
});
