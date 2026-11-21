/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

export const gitHostOauthUnknownFailureText = 'Failed to connect to the Git host. Try again.';

const OAUTH_ERROR_MESSAGES: Record<string, string> = {
  denied: 'The Git host denied access.',
  expired: 'The approval took over 10 minutes, or a different browser session approved the connection. Open the project and start the connection again.',
  failed: gitHostOauthUnknownFailureText,
};

export function oauthErrorTextForValue(value: string): string | undefined {
  return Object.hasOwn(OAUTH_ERROR_MESSAGES, value) ? OAUTH_ERROR_MESSAGES[value] : undefined;
}
