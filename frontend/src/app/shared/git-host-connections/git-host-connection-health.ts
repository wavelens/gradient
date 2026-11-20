/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { GitHostConnectionSummary, TaskCommitStatusPostAttempt, getGitHostConnectionOwnerReadableText } from '@core/models';
import { gitHostLabel } from '@shared/git/git-host';
import { dateTimeText, serverTime } from '@shared/text';

export function statusPostSummary(post: TaskCommitStatusPostAttempt): string {
  if (post.success) return post.response_status ? `Posted · HTTP ${post.response_status}` : 'Posted';
  if (post.error_message) return post.error_message;
  return post.response_status ? `Failed with HTTP ${post.response_status}` : 'Failed';
}

export function gitHostConnectionHost(connection: Pick<GitHostConnectionSummary, 'base_url' | 'git_host_type'>): string {
  if (connection.base_url) {
    try {
      return new URL(connection.base_url).host;
    } catch {
      return connection.base_url;
    }
  }
  return connection.git_host_type === 'github' ? 'github.com' : gitHostLabel(connection.git_host_type);
}

export function gitHostConnectionPickerLabel(connection: GitHostConnectionSummary): string {
  return [
    connection.display_name || connection.name,
    connection.account_login ?? gitHostConnectionHost(connection),
    getGitHostConnectionOwnerReadableText(connection.owner),
  ].join(' · ');
}

export function gitHostConnectionPickerOptions(
  connections: GitHostConnectionSummary[],
  currentId: string | null = null,
): { label: string; value: string }[] {
  const fitting = connections.filter((connection) => connection.repository_fit?.fits !== false);
  const pickableByHand = connections.filter(
    (connection) => connection.repository_fit?.fits === false && (connection.repository_fit.can_be_selected || connection.id === currentId),
  );
  return [
    { label: 'No connection', value: '' },
    ...fitting.map((connection) => ({ value: connection.id, label: gitHostConnectionPickerLabel(connection) })),
    ...pickableByHand.map((connection) => ({
      value: connection.id,
      label: [gitHostConnectionPickerLabel(connection), connection.repository_fit?.reason].filter(Boolean).join(' · '),
    })),
  ];
}

export interface RefusedSignature {
  at: string;
  count: number;
}

export function refusedSignatureNewerThanLastDelivery(connection: GitHostConnectionSummary): RefusedSignature | null {
  const at = connection.last_refused_signature_at;
  if (!at) return null;
  const delivered = connection.last_delivery?.created_at;
  if (delivered && serverTime(delivered).getTime() >= serverTime(at).getTime()) return null;
  return { at, count: connection.refused_signature_count ?? 1 };
}

export interface AccessTokenState {
  text: string;
  expired: boolean;
}

export function accessTokenState(connection: GitHostConnectionSummary, now: number): AccessTokenState | null {
  if (connection.access_token_refreshes_automatically) return { text: 'Token refreshes automatically', expired: false };
  const expiresAt = connection.access_token_expires_at;
  if (!expiresAt) return null;
  return serverTime(expiresAt).getTime() <= now
    ? { text: 'Token expired', expired: true }
    : { text: `Token expires ${dateTimeText(expiresAt)}`, expired: false };
}
