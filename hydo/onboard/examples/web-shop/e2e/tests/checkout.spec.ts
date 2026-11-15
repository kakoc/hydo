import { expect, test } from '@playwright/test';

test('places an order', async ({ page }) => {
  await page.goto('/');
  await page.getByRole('button', { name: 'Add Coffee mug to cart' }).click();
  await expect(page.getByTestId('cart-count')).toHaveText('1');
  await page.getByRole('link', { name: /Cart/ }).click();
  await expect(page.getByTestId('total')).toHaveText('Total $12.00');
  await page.getByRole('button', { name: 'Place order' }).click();
  await expect(page.getByTestId('order-title')).toHaveText(/^Order o\d+ confirmed$/);
  await expect(page.getByText('Total $12.00')).toBeVisible();
});
