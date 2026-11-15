/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { expect, test as setup } from '@playwright/test';
import {
  fillUsernameAndPasswordAndClickSignIn,
  ordinaryMemberTestAccount,
  superuserTestAccount,
} from './test-accounts-storage-state-and-entity-names';

for (const account of [superuserTestAccount, ordinaryMemberTestAccount]) {
  setup(`sign in as ${account.username}`, async ({ page }) => {
    await page.goto('/account/login');
    await fillUsernameAndPasswordAndClickSignIn(page, account);
    await expect(page).toHaveTitle(/^Dashboard/);
    await page.context().storageState({ path: account.storageState });
  });
}
