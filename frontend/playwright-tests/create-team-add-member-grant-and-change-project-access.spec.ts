/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { expect, test, type Page } from '@playwright/test';
import {
  createProjectThroughDialogAndReturnSlug,
  createUniqueTestEntityName,
  ordinaryMemberTestAccount,
} from './test-accounts-storage-state-and-entity-names';

async function createTeamAndReturnUrlName(page: Page): Promise<string> {
  const team = createUniqueTestEntityName('pw-team');
  await page.goto('/teams');
  await page.getByRole('button', { name: 'Create Team' }).click();
  const dialog = page.getByRole('dialog', { name: 'Create Team' });
  await dialog.getByRole('textbox', { name: 'Display Name' }).fill(team);
  await dialog.getByRole('button', { name: 'Create' }).click();
  await expect(page).toHaveURL(`/team/${team}`);
  return team;
}

test('a team member sees a private project once the team is granted access to it', async ({ page, browser }) => {
  const team = await createTeamAndReturnUrlName(page);
  const project = await createProjectThroughDialogAndReturnSlug(page);

  const memberContext = await browser.newContext({ storageState: ordinaryMemberTestAccount.storageState });
  const memberPage = await memberContext.newPage();
  const projectLinkForMember = memberPage.getByRole('link', { name: new RegExp(project) });
  await memberPage.goto('/projects');
  await expect(memberPage.getByRole('heading', { level: 1, name: 'Projects' })).toBeVisible();
  await expect(projectLinkForMember).toHaveCount(0);

  await page.goto(`/team/${team}/members/people`);
  await page.getByRole('button', { name: 'Invite Member' }).click();
  const inviteDialog = page.getByRole('dialog', { name: 'Invite Member' });
  await inviteDialog.getByRole('combobox', { name: 'Username' }).fill(ordinaryMemberTestAccount.username);
  await page.getByRole('option', { name: ordinaryMemberTestAccount.username, exact: true }).click();
  await inviteDialog.getByRole('radio', { name: 'Add now' }).check();
  await inviteDialog.getByRole('button', { name: 'Add', exact: true }).click();
  await expect(page.getByRole('combobox', { name: `Role: ${ordinaryMemberTestAccount.username}` })).toContainText('Member');

  await page.goto(`/project/${project}/members/teams`);
  await page.getByRole('button', { name: 'Grant Team' }).click();
  const grantDialog = page.getByRole('dialog', { name: 'Grant Team' });
  await grantDialog.getByRole('combobox', { name: 'Team' }).fill(team);
  await page.getByRole('option', { name: new RegExp(`^${team} `) }).click();
  await grantDialog.getByRole('button', { name: 'Grant' }).click();
  await expect(page.getByRole('link', { name: new RegExp(`^Open team ${team}`) })).toBeVisible();

  await memberPage.reload();
  await expect(projectLinkForMember).toBeVisible();
  await memberContext.close();
});

test('the workers switch of a team grant can be turned on and stays on', async ({ page }) => {
  const team = await createTeamAndReturnUrlName(page);
  const project = await createProjectThroughDialogAndReturnSlug(page);

  await page.goto(`/project/${project}/members/teams`);
  await page.getByRole('button', { name: 'Grant Team' }).click();
  const grantDialog = page.getByRole('dialog', { name: 'Grant Team' });
  await grantDialog.getByRole('combobox', { name: 'Team' }).fill(team);
  await page.getByRole('option', { name: new RegExp(`^${team} `) }).click();
  await grantDialog.getByRole('button', { name: 'Grant' }).click();

  const workersSwitch = page.getByRole('checkbox', { name: 'Workers' });
  await expect(workersSwitch).not.toBeChecked();
  await workersSwitch.check();
  await expect(page).toHaveURL(`/project/${project}/members/teams`);
  await expect(workersSwitch).toBeChecked();
  await expect(workersSwitch).toBeEnabled();

  await page.reload();
  await expect(workersSwitch).toBeChecked();
});
