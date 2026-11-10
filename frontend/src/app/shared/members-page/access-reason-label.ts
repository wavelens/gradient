/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import type { PersonAccessReason, PersonWithAccessToProjectOrCache, TeamNameAndDisplayName } from '@core/models';

export interface PeopleListRow {
  user: string;
  displayName: string;
  role: string | null;
  directRole: string | null;
  locked: boolean;
  reasons: readonly PersonAccessReason[];
  sources: readonly { label: string; team: string | null }[];
}

export type AddNowOrInviteChoice = 'add-now' | 'invite';

const teamName = (team: TeamNameAndDisplayName | null | undefined): string => team?.display_name || team?.name || '';

const isTeamReason = (reason: PersonAccessReason): boolean => reason.kind === 'team' || reason.kind === 'owner';

export function getAccessReasonLabel(reason: PersonAccessReason): string {
  const base = {
    direct: 'Direct',
    team: `From ${teamName(reason.team)}`,
    owner: `From ${teamName(reason.team)} (owner)`,
    superuser: 'Instance administrator',
  }[reason.kind];
  const applying = reason.applies ? base : `${base}, overridden by direct role`;
  return reason.managed ? `${applying} · server configuration` : applying;
}

function buildPeopleListRowFromPersonAndAccessReasons(
  person: Pick<PeopleListRow, 'user' | 'displayName' | 'role'>,
  reasons: readonly PersonAccessReason[],
): PeopleListRow {
  const direct = reasons.find((reason) => reason.kind === 'direct');
  return {
    ...person,
    directRole: direct?.role ?? null,
    locked: direct?.managed ?? false,
    reasons,
    sources: reasons.map((reason) => ({ label: getAccessReasonLabel(reason), team: reason.team?.name ?? null })),
  };
}

export function toPeopleListRowFromAccessPerson(person: PersonWithAccessToProjectOrCache): PeopleListRow {
  return buildPeopleListRowFromPersonAndAccessReasons({ user: person.user, displayName: person.user_display_name, role: person.role ?? null }, person.reasons);
}

export function copyPeopleRowAfterRoleSave(row: PeopleListRow, role: string): PeopleListRow {
  const direct: PersonAccessReason = { kind: 'direct', role, applies: true, managed: false };
  const others = row.reasons
    .filter((reason) => reason.kind !== 'direct')
    .map((reason) => (isTeamReason(reason) ? { ...reason, applies: false } : reason));
  return buildPeopleListRowFromPersonAndAccessReasons({ user: row.user, displayName: row.displayName, role }, [direct, ...others]);
}

export function getTeamsProvidingAccessAfterDirectRoleRemoval(row: Pick<PeopleListRow, 'reasons'>): TeamNameAndDisplayName[] {
  return row.reasons.filter(isTeamReason).flatMap((reason) => (reason.team ? [reason.team] : []));
}

export function getRemoveMemberDialogSentence(
  place: 'project' | 'cache',
  row: Pick<PeopleListRow, 'user' | 'reasons'>,
  signedInUser: boolean,
): string {
  const teamReasons = row.reasons.filter(isTeamReason);
  if (teamReasons.length) {
    const roles = teamReasons.map((reason) => `${reason.role ?? 'access'} through ${teamName(reason.team)}`).join(' and ');
    return signedInUser ? `You keep ${roles}.` : `Roles for ${row.user} still include ${roles}.`;
  }
  if (row.reasons.some((reason) => reason.kind === 'superuser')) {
    return signedInUser ? 'You keep access as an instance administrator.' : `${row.user} can keep access as an instance administrator.`;
  }
  return signedInUser ? `You lose all access to this ${place}.` : `${row.user} can no longer access any part of this ${place}.`;
}

export function getInviteDialogPreviewSentence(user: string, role: string, choice: AddNowOrInviteChoice): string {
  return choice === 'add-now' ? `${user} gets ${role} now.` : `Send ${user} an invitation for ${role}.`;
}

export function usernameHasTeamGrantOrOwnerTeamAccessToProjectOrCache(rows: readonly PeopleListRow[], user: string): boolean {
  return rows.some((row) => row.user === user && row.reasons.some(isTeamReason));
}
