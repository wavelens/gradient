/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { randomBytes } from 'node:crypto';
import { join } from 'node:path';
import { expect, type Locator, type Page } from '@playwright/test';

const storageStateDirectory = join(__dirname, 'signed-in-browser-storage-states');

export const superuserTestAccount = {
  username: 'stateadmin',
  password: 'admin_password',
  storageState: join(storageStateDirectory, 'superuser.json'),
};

export const ordinaryMemberTestAccount = {
  username: 'statemember',
  password: 'admin_password',
  storageState: join(storageStateDirectory, 'member.json'),
};

export function createUniqueTestEntityName(prefix: string): string {
  return `${prefix}-${randomBytes(4).toString('hex')}`;
}

export function getListRowByEntityName(page: Page, name: string): Locator {
  return page.locator('gr-row', { hasText: name });
}

export async function fillUsernameAndPasswordAndClickSignIn(
  page: Page,
  account: { username: string; password: string },
): Promise<void> {
  await page.getByLabel('Username or Email').fill(account.username);
  await page.getByLabel('Password', { exact: true }).fill(account.password);
  await page.getByRole('button', { name: 'Sign In' }).click();
}

export async function createProjectThroughDialogAndReturnSlug(page: Page): Promise<string> {
  const slug = createUniqueTestEntityName('pw-project');
  await page.goto('/projects');
  await page.getByRole('button', { name: 'Create Project' }).click();
  const dialog = page.getByRole('dialog', { name: 'Create Project' });
  await dialog.getByRole('textbox', { name: 'Display Name' }).fill(slug);
  await expect(dialog.getByRole('textbox', { name: 'Name', exact: true })).toHaveValue(slug);
  await dialog.getByRole('button', { name: 'Create' }).click();
  await expect(page).toHaveURL(`/project/${slug}`);
  await expect(page.getByRole('heading', { level: 1, name: slug })).toBeVisible();
  return slug;
}
