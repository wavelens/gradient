/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import type { AutoCompleteSuggestion } from '@gradient/ui/ui';
import type { UserSearchResult } from '@core/models';

export function formatMemberNameForConfirmDialog(displayName: string | null | undefined, username: string): string {
  return displayName && displayName !== username ? `${displayName} (${username})` : username;
}

export function formatMemberNameForAutocompleteSuggestion(user: UserSearchResult): AutoCompleteSuggestion {
  return { label: formatMemberNameForConfirmDialog(user.name, user.username), value: user.username };
}
