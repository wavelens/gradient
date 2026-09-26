/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

export interface EventEnvelope<C = Record<string, unknown>> {
  event: string;
  at: string;
  content: C;
}
