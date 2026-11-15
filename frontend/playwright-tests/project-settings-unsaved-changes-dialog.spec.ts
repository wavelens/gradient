/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { expect, test } from '@playwright/test';
import { createProjectThroughDialogAndReturnSlug } from './test-accounts-storage-state-and-entity-names';

test('a new project is listed and opens from the project list', async ({ page }) => {
  const slug = await createProjectThroughDialogAndReturnSlug(page);
  await page.goto('/projects');
  await page.getByRole('link', { name: new RegExp(slug) }).click();
  await expect(page).toHaveURL(`/project/${slug}`);
  await expect(page.getByRole('heading', { level: 2, name: 'No tasks yet' })).toBeVisible();
});

test('edited project settings persist across a reload', async ({ page }) => {
  const slug = await createProjectThroughDialogAndReturnSlug(page);
  await page.getByRole('main').getByRole('link', { name: 'Settings' }).click();
  await expect(page).toHaveURL(`/project/${slug}/settings`);

  const saveButton = page.getByRole('button', { name: 'Save Changes' });
  await expect(saveButton).toBeDisabled();
  await page.getByRole('textbox', { name: 'Description' }).fill('Edited by the browser test');
  await saveButton.click();
  await expect(page.getByRole('status').filter({ hasText: 'Project settings saved' })).toBeVisible();

  await page.reload();
  await expect(page.getByRole('textbox', { name: 'Description' })).toHaveValue('Edited by the browser test');
});

test('the unsaved changes dialog keeps a dirty settings form until the user leaves', async ({ page }) => {
  const slug = await createProjectThroughDialogAndReturnSlug(page);
  await page.goto(`/project/${slug}/settings`);
  await page.getByRole('textbox', { name: 'Description' }).fill('Never saved');

  const breadcrumbToProjects = page.getByRole('navigation', { name: 'Breadcrumb' }).getByRole('link', { name: 'Projects' });
  const leaveDialog = page.getByRole('dialog', { name: 'Leave Without Saving' });

  await breadcrumbToProjects.click();
  await leaveDialog.getByRole('button', { name: 'Cancel' }).click();
  await expect(leaveDialog).toBeHidden();
  await expect(page).toHaveURL(`/project/${slug}/settings`);
  await expect(page.getByRole('textbox', { name: 'Description' })).toHaveValue('Never saved');

  await breadcrumbToProjects.click();
  await leaveDialog.getByRole('button', { name: 'Leave' }).click();
  await expect(page).toHaveURL('/projects');

  await page.goto(`/project/${slug}/settings`);
  await expect(page.getByRole('textbox', { name: 'Description' })).toHaveValue('');
});
