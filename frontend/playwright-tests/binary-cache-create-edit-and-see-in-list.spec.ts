/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { expect, test } from '@playwright/test';
import { createUniqueTestEntityName } from './test-accounts-storage-state-and-entity-names';

test('a new cache is listed, opens and keeps edited settings', async ({ page }) => {
  const cache = createUniqueTestEntityName('pw-cache');
  await page.goto('/caches');
  await page.getByRole('button', { name: 'Create Cache' }).click();
  const dialog = page.getByRole('dialog', { name: 'Create Cache' });
  await dialog.getByRole('textbox', { name: 'Display Name' }).fill(cache);
  await expect(dialog.getByRole('textbox', { name: 'Name', exact: true })).toHaveValue(cache);
  await dialog.getByRole('button', { name: 'Create' }).click();

  await expect(page).toHaveURL(`/caches/${cache}`);
  await expect(page.getByRole('textbox', { name: 'nix.conf snippet' })).toHaveValue(new RegExp(`/cache/${cache}`));

  await page.getByRole('main').getByRole('link', { name: 'Settings' }).click();
  await expect(page).toHaveURL(`/caches/${cache}/settings`);
  await page.getByRole('textbox', { name: 'Description' }).fill('Edited by the browser test');
  await page.getByRole('spinbutton', { name: 'Priority', exact: true }).fill('42');
  await page.getByRole('button', { name: 'Save Changes' }).click();
  await expect(page.getByRole('button', { name: 'Save Changes' })).toBeDisabled();

  await page.reload();
  await expect(page.getByRole('textbox', { name: 'Description' })).toHaveValue('Edited by the browser test');
  await expect(page.getByRole('spinbutton', { name: 'Priority', exact: true })).toHaveValue('42');

  await page.goto('/caches');
  await page.getByRole('link', { name: new RegExp(cache) }).click();
  await expect(page).toHaveURL(`/caches/${cache}`);
});
