/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { MissingPermissionLoadStateError } from '@gradient/ui/ui';

export function createMissingPermissionLoadStateError(owner: 'project' | 'cache', isViewerMemberOfOwner = true): MissingPermissionLoadStateError {
  return createNoAccessLoadStateError(owner, isViewerMemberOfOwner ? 'admins' : 'members');
}

export function createNoAccessLoadStateError(owner: 'project' | 'cache' | 'team', group: 'members' | 'admins'): MissingPermissionLoadStateError {
  return new MissingPermissionLoadStateError(`Only the ${group} of this ${owner} can view this. Ask an admin of this ${owner} for access.`);
}
