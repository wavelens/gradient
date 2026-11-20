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
