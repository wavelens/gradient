/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

export function formatMemberNameForConfirmDialog(displayName: string | null | undefined, username: string): string {
  return displayName && displayName !== username ? `${displayName} (${username})` : username;
}
