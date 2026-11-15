/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { defineConfig, devices } from '@playwright/test';
import { superuserTestAccount } from './test-accounts-storage-state-and-entity-names';

export default defineConfig({
  testDir: '.',
  outputDir: 'test-results',
  forbidOnly: true,
  // Rate limits on the server apply to API calls per user and to sign-in per client address. All specs share two accounts, so the specs run one at a time.
  workers: 1,
  reporter: 'list',
  use: {
    baseURL: process.env['GRADIENT_WEB_UI_BASE_URL'] ?? 'http://localhost:4200',
    trace: 'retain-on-failure',
  },
  projects: [
    { name: 'setup', testMatch: /\.setup\.ts$/ },
    {
      name: 'chromium',
      dependencies: ['setup'],
      use: { ...devices['Desktop Chrome'], storageState: superuserTestAccount.storageState },
    },
  ],
});
