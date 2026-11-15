/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { expect, test } from '@playwright/test';
import { fillUsernameAndPasswordAndClickSignIn, superuserTestAccount } from './test-accounts-storage-state-and-entity-names';

test.use({ storageState: { cookies: [], origins: [] } });

test('a wrong password shows an error and keeps the user on the sign-in page', async ({ page }) => {
  await page.goto('/account/login');
  await fillUsernameAndPasswordAndClickSignIn(page, { ...superuserTestAccount, password: 'not-the-password' });
  await expect(page.getByRole('alert')).toContainText('Invalid credentials');
  await expect(page).toHaveTitle(/^Sign In/);
});

test('a guarded page sends a signed-out user to sign in and back and signing out ends the session', async ({ page }) => {
  await page.goto('/settings/keys');
  await expect(page).toHaveURL('/account/login?next=%2Fsettings%2Fkeys');
  await fillUsernameAndPasswordAndClickSignIn(page, superuserTestAccount);
  await expect(page).toHaveURL('/settings/keys');
  await expect(page.getByRole('heading', { level: 1, name: 'API Keys' })).toBeVisible();

  await page.getByRole('button', { name: superuserTestAccount.username }).click();
  await page.getByRole('menuitem', { name: 'Log out' }).click();
  await expect(page).toHaveURL('/account/login');

  await page.goto('/teams');
  await expect(page).toHaveURL('/account/login?next=%2Fteams');
});
