import { defineConfig, devices } from '@playwright/test';

// An ordinary Playwright configuration, as a team writes it for its own machines and its CI.
// hydo runs it unchanged inside the guest.
export default defineConfig({
  testDir: './tests',
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 2 : 0,
  reporter: process.env.CI ? 'github' : 'html',
  use: {
    baseURL: 'http://localhost:3000',
    trace: 'on-first-retry',
  },
  projects: [{ name: 'chromium', use: { ...devices['Desktop Chrome'] } }],
  webServer: {
    command: 'node ../app/serve.mjs',
    url: 'http://localhost:3000',
    reuseExistingServer: !process.env.CI,
  },
});
