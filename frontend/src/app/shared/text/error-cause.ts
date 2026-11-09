/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

/// Error chains such as `send HTTP request: dns error: Name or service not known` end with the cause that a user can address.
export function getLastErrorCause(chain: string): string {
  return chain.split(': ').at(-1)?.trim() ?? '';
}
