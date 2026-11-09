/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

export const PENDING_INVITATION_REVOKE_FAILURE = "Couldn't revoke invitation";

export function getPendingInvitationRevocationConsequenceSentence(username: string): string {
  return `The invitation for ${username} stops working. An admin can invite ${username} again later.`;
}
