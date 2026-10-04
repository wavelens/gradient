/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

/// Chart axes have little room: a worker without a display name is cut to the head of its id.
export function workerAxisLabel(name: string | null, id: string | null): string {
  return name || (id ?? '-').slice(0, 12);
}
