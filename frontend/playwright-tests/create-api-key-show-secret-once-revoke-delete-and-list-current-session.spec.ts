/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { expect, test, type Page } from '@playwright/test';
import { createUniqueTestEntityName, getListRowByEntityName } from './test-accounts-storage-state-and-entity-names';

async function createApiKeyThroughDialogAndReadSecret(page: Page, name: string): Promise<string> {
  await page.goto('/settings/keys');
  await page.getByRole('main').getByRole('button', { name: 'Create API Key' }).first().click();
  const createDialog = page.getByRole('dialog', { name: 'Create API Key' });
  await createDialog.getByRole('textbox', { name: 'Name' }).fill(name);
  await createDialog.getByRole('button', { name: 'Create' }).click();

  const createdDialog = page.getByRole('dialog', { name: 'API Key Created' });
  await expect(createdDialog).toContainText('it will not be shown again');
  const secret = createdDialog.getByRole('textbox', { name: 'New API key' });
  await expect(secret).toHaveValue(/^GRAD\S{20,}$/);
  const value = await secret.inputValue();
  await createdDialog.getByRole('button', { name: 'Done' }).click();
  await expect(createdDialog).toBeHidden();
  return value;
}

test('a new API key shows its secret once', async ({ page }) => {
  const name = createUniqueTestEntityName('pw-key');
  const secret = await createApiKeyThroughDialogAndReadSecret(page, name);

  await expect(getListRowByEntityName(page, name)).toContainText('Last used: never');
  await page.reload();
  await expect(getListRowByEntityName(page, name)).toBeVisible();
  await expect(page.getByText(secret)).toHaveCount(0);
  const fieldValues = await page
    .locator('input, textarea')
    .evaluateAll((fields) => fields.map((field) => (field as HTMLInputElement).value));
  expect(fieldValues).not.toContain(secret);
});

test('a revoked API key is marked revoked and a deleted one leaves the list', async ({ page }) => {
  const name = createUniqueTestEntityName('pw-key');
  await createApiKeyThroughDialogAndReadSecret(page, name);
  const row = getListRowByEntityName(page, name);

  await row.getByRole('button', { name: 'Revoke' }).click();
  await page.getByRole('dialog', { name: 'Revoke API key' }).getByRole('button', { name: 'Revoke' }).click();
  await expect(row).toContainText('Revoked');
  await expect(row.getByRole('button', { name: 'Revoke' })).toHaveCount(0);

  await row.getByRole('button', { name: 'Delete' }).click();
  await page.getByRole('dialog').getByRole('button', { name: 'Delete' }).click();
  await expect(row).toHaveCount(0);
  await page.reload();
  await expect(page.getByRole('heading', { level: 1, name: 'API Keys' })).toBeVisible();
  await expect(row).toHaveCount(0);
});

test('the sessions page marks the current session as this device', async ({ page }) => {
  await page.goto('/settings/sessions');
  const currentSession = getListRowByEntityName(page, 'This device');
  await expect(currentSession).toHaveCount(1);
  await expect(currentSession.getByRole('button', { name: 'Log out' })).toBeVisible();
});
