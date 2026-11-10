/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import type { PersonAccessReason, PersonWithAccessToProjectOrCache } from '@core/models';
import {
  copyPeopleRowAfterRoleSave,
  getAccessReasonLabel,
  getInviteDialogPreviewSentence,
  getRemoveMemberDialogSentence,
  getTeamsProvidingAccessAfterDirectRoleRemoval,
  toPeopleListRowFromAccessPerson,
  usernameHasTeamGrantOrOwnerTeamAccessToProjectOrCache,
} from './access-reason-label';

const alice = { name: 'alice-team', display_name: 'Alice Team' };
const direct = (role: string, extra: Partial<PersonAccessReason> = {}): PersonAccessReason => ({ kind: 'direct', role, applies: true, managed: false, ...extra });
const fromTeam = (role: string, applies = true): PersonAccessReason => ({ kind: 'team', team: alice, role, applies, managed: false });

function person(reasons: PersonAccessReason[], role: string | null = 'View'): PersonWithAccessToProjectOrCache {
  return { user: 'carol', user_display_name: 'Carol', role, permissions: [], reasons };
}

describe('access reason labels', () => {
  it('names every source of access a person has', () => {
    expect(getAccessReasonLabel(direct('Write'))).toBe('Direct');
    expect(getAccessReasonLabel(fromTeam('View'))).toBe('From Alice Team');
    expect(getAccessReasonLabel({ kind: 'owner', team: alice, role: 'Admin', applies: true, managed: false })).toBe('From Alice Team (owner)');
    expect(getAccessReasonLabel({ kind: 'superuser', applies: true, managed: false })).toBe('Instance administrator');
  });

  it('marks team access that a direct role overrides and rows from the server configuration', () => {
    expect(getAccessReasonLabel(fromTeam('View', false))).toBe('From Alice Team, overridden by direct role');
    expect(getAccessReasonLabel(direct('Admin', { managed: true }))).toBe('Direct · server configuration');
  });

  it('locks rows whose direct role comes from the server configuration', () => {
    const row = toPeopleListRowFromAccessPerson(person([direct('Admin', { managed: true }), fromTeam('View', false)], 'Admin'));
    expect(row).toEqual(expect.objectContaining({ user: 'carol', displayName: 'Carol', role: 'Admin', directRole: 'Admin', locked: true }));
    expect(row.sources).toEqual([
      { label: 'Direct · server configuration', team: null },
      { label: 'From Alice Team, overridden by direct role', team: 'alice-team' },
    ]);
  });

  it('turns team access into overridden access once a direct role is saved', () => {
    const row = copyPeopleRowAfterRoleSave(toPeopleListRowFromAccessPerson(person([fromTeam('View')])), 'Write');
    expect(row.role).toBe('Write');
    expect(row.directRole).toBe('Write');
    expect(row.sources.map((source) => source.label)).toEqual(['Direct', 'From Alice Team, overridden by direct role']);
  });
});

describe('remove member sentence', () => {
  it('names the team access that remains after the direct role goes', () => {
    const row = toPeopleListRowFromAccessPerson(person([direct('Write'), fromTeam('View', false)]));
    expect(getRemoveMemberDialogSentence('project', row, false)).toBe('Roles for carol still include View through Alice Team.');
    expect(getRemoveMemberDialogSentence('project', row, true)).toBe('You keep View through Alice Team.');
    expect(getTeamsProvidingAccessAfterDirectRoleRemoval(row)).toEqual([alice]);
  });

  it('says when a person keeps access only as an instance administrator', () => {
    const row = toPeopleListRowFromAccessPerson(person([direct('Write'), { kind: 'superuser', applies: true, managed: false }]));
    expect(getRemoveMemberDialogSentence('cache', row, false)).toBe('carol can keep access as an instance administrator.');
    expect(getTeamsProvidingAccessAfterDirectRoleRemoval(row)).toEqual([]);
  });

  it('says when a person loses all access', () => {
    const row = toPeopleListRowFromAccessPerson(person([direct('Write')]));
    expect(getRemoveMemberDialogSentence('cache', row, false)).toBe('carol can no longer access any part of this cache.');
    expect(getRemoveMemberDialogSentence('project', row, true)).toBe('You lose all access to this project.');
  });
});

describe('invite dialog helpers', () => {
  it('previews what the submit does', () => {
    expect(getInviteDialogPreviewSentence('carol', 'Write', 'add-now')).toBe('carol gets Write now.');
    expect(getInviteDialogPreviewSentence('carol', 'Write', 'invite')).toBe('Send carol an invitation for Write.');
  });

  it('offers adding now only for people who already reach the place through a team', () => {
    const rows = [
      toPeopleListRowFromAccessPerson(person([fromTeam('View')])),
      toPeopleListRowFromAccessPerson({ ...person([direct('Write')]), user: 'dave' }),
    ];
    expect(usernameHasTeamGrantOrOwnerTeamAccessToProjectOrCache(rows, 'carol')).toBe(true);
    expect(usernameHasTeamGrantOrOwnerTeamAccessToProjectOrCache(rows, 'dave')).toBe(false);
    expect(usernameHasTeamGrantOrOwnerTeamAccessToProjectOrCache(rows, 'erin')).toBe(false);
  });
});
