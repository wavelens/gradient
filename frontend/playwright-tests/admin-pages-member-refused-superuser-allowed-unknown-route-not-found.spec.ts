/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { expect, test } from '@playwright/test';
import { ordinaryMemberTestAccount } from './test-accounts-storage-state-and-entity-names';

test('a superuser opens the administration pages', async ({ page }) => {
  await page.goto('/admin');
  await expect(page).toHaveURL('/admin/overview');
  await expect(page).toHaveTitle(/^Overview · Administration/);
  await expect(page.getByRole('navigation', { name: 'Main' }).getByRole('link', { name: 'Admin' })).toBeVisible();
});

test('an unknown address shows the not-found page', async ({ page }) => {
  await page.goto('/no/such/page');
  await expect(page.getByRole('heading', { level: 1, name: 'Page Not Found' })).toBeVisible();
  await expect(page.getByRole('link', { name: 'Go Home' })).toHaveAttribute('href', '/');
});

test.describe('as a member without superuser rights', () => {
  test.use({ storageState: ordinaryMemberTestAccount.storageState });

  test('the administration pages are refused', async ({ page }) => {
    await page.goto('/admin');
    await expect(page.getByRole('heading', { level: 1, name: 'Admin Only' })).toBeVisible();
    await expect(page.getByText(`Signed in as ${ordinaryMemberTestAccount.username}.`)).toBeVisible();
    await expect(page.getByRole('navigation', { name: 'Main' }).getByRole('link', { name: 'Admin' })).toHaveCount(0);
  });

  test('the user access administration page is refused', async ({ page }) => {
    await page.goto('/admin/users');
    await expect(page.getByRole('heading', { level: 1, name: 'Admin Only' })).toBeVisible();
  });
});
