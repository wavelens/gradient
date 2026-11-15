/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { expect, test, type Page } from '@playwright/test';
import {
  createProjectThroughDialogAndReturnSlug,
  createUniqueTestEntityName,
} from './test-accounts-storage-state-and-entity-names';

async function createProjectThroughConnectRepositoryWizardAndReturnNewTaskPagePath(page: Page): Promise<string> {
  const project = await createProjectThroughDialogAndReturnSlug(page);
  const task = createUniqueTestEntityName('pw-task');
  const main = page.getByRole('main');

  await main.getByRole('link', { name: 'Connect repository' }).click();
  await main.getByRole('textbox', { name: 'Repository URL' }).fill('https://git.example.com/acme/app.git');
  await main.getByRole('textbox', { name: 'Display Name' }).fill(task);
  await expect(main.getByRole('textbox', { name: 'Name', exact: true })).toHaveValue(task);
  await main.getByRole('button', { name: 'Continue' }).click();

  await expect(main.getByRole('heading', { level: 2, name: 'Repository access' })).toBeVisible();
  await main.getByRole('button', { name: 'Continue' }).click();

  await expect(main.getByRole('heading', { level: 2, name: 'Triggers and Settings' })).toBeVisible();
  await main.getByRole('button', { name: 'Create task' }).click();

  const taskPagePath = `/project/${project}/task/${task}`;
  await expect(page).toHaveURL(taskPagePath);
  await expect(page.getByRole('heading', { level: 1, name: task })).toBeVisible();
  return taskPagePath;
}

test('the connect repository wizard creates a task', async ({ page }) => {
  await createProjectThroughConnectRepositoryWizardAndReturnNewTaskPagePath(page);
  await expect(page.getByRole('link', { name: 'https://git.example.com/acme/app.git' })).toBeVisible();
  await expect(page.getByRole('heading', { level: 2, name: 'No evaluations yet' })).toBeVisible();
});

test('task settings refuse a value above the server limit and persist a valid edit', async ({ page }) => {
  const taskPagePath = await createProjectThroughConnectRepositoryWizardAndReturnNewTaskPagePath(page);
  await page.goto(`${taskPagePath}/settings`);

  const keepEvaluations = page.getByRole('spinbutton', { name: 'Keep Evaluations' });
  const saveButton = page.getByRole('button', { name: 'Save Changes' });
  await page.getByRole('textbox', { name: 'Description' }).fill('Edited by the browser test');
  await keepEvaluations.fill('9999');
  await expect(page.getByRole('alert').filter({ hasText: /can keep at most \d+ evaluations per task/ })).toBeVisible();
  await expect(saveButton).toBeDisabled();

  await keepEvaluations.fill('3');
  await saveButton.click();
  await expect(page.getByRole('status').filter({ hasText: 'Task settings saved' })).toBeVisible();

  await page.reload();
  await expect(keepEvaluations).toHaveValue('3');
  await expect(page.getByRole('textbox', { name: 'Description' })).toHaveValue('Edited by the browser test');
});

test('the trigger form refuses an invalid cron expression and keeps a valid schedule', async ({ page }) => {
  const taskPagePath = await createProjectThroughConnectRepositoryWizardAndReturnNewTaskPagePath(page);
  await page.goto(`${taskPagePath}/triggers`);

  await page.getByRole('button', { name: 'New Trigger' }).click();
  const dialog = page.getByRole('dialog', { name: 'New Trigger' });
  await dialog.getByRole('combobox', { name: 'Type' }).click();
  await page.getByRole('option', { name: 'Schedule (cron)' }).click();

  const cron = dialog.getByRole('textbox', { name: 'Cron Expression' });
  await cron.fill('not a cron');
  await dialog.getByRole('button', { name: 'Create' }).click();
  await expect(dialog.getByRole('alert')).toContainText('Enter 5, 6, or 7 fields');

  await cron.fill('0 3 * * *');
  await dialog.getByRole('button', { name: 'Create' }).click();
  await expect(dialog).toBeHidden();

  const schedule = page.getByText('Every day at 03:00 UTC');
  await expect(schedule).toBeVisible();
  await page.reload();
  await expect(schedule).toBeVisible();
});

test('the action form requires a name, a complete URL and an event and keeps a valid action', async ({ page }) => {
  const taskPagePath = await createProjectThroughConnectRepositoryWizardAndReturnNewTaskPagePath(page);
  await page.goto(`${taskPagePath}/actions`);

  await page.getByRole('button', { name: 'New Action' }).click();
  const dialog = page.getByRole('dialog', { name: 'New Action' });
  await dialog.getByRole('button', { name: 'Create' }).click();
  await expect(dialog.getByRole('alert')).toHaveText([
    'Enter a name for the action.',
    'Enter a URL.',
    'Select at least one event.',
  ]);

  await dialog.getByRole('textbox', { name: 'Name' }).fill('notify-on-failure');
  await dialog.getByRole('textbox', { name: 'URL' }).fill('not a url');
  await dialog.getByRole('group', { name: 'Evaluation' }).getByRole('checkbox', { name: 'Failed' }).check();
  await dialog.getByRole('button', { name: 'Create' }).click();
  await expect(dialog.getByRole('alert')).toHaveText(['Enter a complete URL with the https:// or http:// prefix.']);

  await dialog.getByRole('textbox', { name: 'URL' }).fill('https://hooks.example.com/gradient');
  await dialog.getByRole('button', { name: 'Create' }).click();
  await expect(dialog).toBeHidden();

  await expect(page.getByText('notify-on-failure', { exact: true })).toBeVisible();
  await page.reload();
  await expect(page.getByText('notify-on-failure', { exact: true })).toBeVisible();
  await expect(page.getByText('Evaluation failed', { exact: true })).toBeVisible();
});
