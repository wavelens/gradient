/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

export interface AccessState {
  managed: boolean;
  canEdit: boolean;
}

export function accessFromEntity(e: {
  managed: boolean;
  can_edit: boolean;
}): AccessState {
  return { managed: e.managed, canEdit: e.can_edit };
}
