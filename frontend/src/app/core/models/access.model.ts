/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

export interface AccessState {
  /// True when the resource is managed by declarative state. Config edits are
  /// blocked even when the caller has permission, but trigger-style actions
  /// (start/abort evaluations) remain available.
  managed: boolean;
  /// True when the caller may edit configuration (Permission::EditProject /
  /// Write or Admin org role / etc.).
  canEdit: boolean;
  /// True when the caller may run trigger-style actions on the resource. For
  /// projects this is Permission::TriggerEvaluation. For resources without a
  /// distinct trigger permission (caches, orgs) this mirrors `canEdit`.
  canTrigger: boolean;
}

/// Lifts a backend entity into an `AccessState`. `can_trigger` is optional on
/// the wire — entities without their own trigger permission (caches, orgs)
/// fall back to `can_edit`, preserving the previous single-permission model.
export function accessFromEntity(e: {
  managed: boolean;
  can_edit: boolean;
  can_trigger?: boolean;
}): AccessState {
  return {
    managed: e.managed,
    canEdit: e.can_edit,
    canTrigger: e.can_trigger ?? e.can_edit,
  };
}
