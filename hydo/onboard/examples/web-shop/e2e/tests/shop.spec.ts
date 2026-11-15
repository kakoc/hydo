import { expect, test } from '@playwright/test';

test('lists the products', async ({ page }) => {
  await page.goto('/');
  await expect(page.getByTestId('product-name')).toHaveText(['Coffee mug', 'Tea pot', 'Notebook']);
});

// A flaky test, written the way such tests are often written: it waits a fixed time for the
// cart to update and then reads the count once. It passes while the API answers quickly.
test('adds two products to the cart', async ({ page }) => {
  await page.goto('/');
  await page.getByRole('button', { name: 'Add Coffee mug to cart' }).click();
  await page.getByRole('button', { name: 'Add Tea pot to cart' }).click();
  await page.waitForTimeout(250);
  expect(await page.getByTestId('cart-count').textContent()).toBe('2');
});
