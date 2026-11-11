/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import type { GitHostConnectionOwner } from './api.generated';

export type GitHostType = 'gitea' | 'forgejo' | 'gitlab' | 'github';

export function getGitHostConnectionOwnerReadableText(owner: GitHostConnectionOwner): string {
  switch (owner.type) {
    case 'instance':
      return 'Instance';
    case 'team':
      return `Team ${owner.name}`;
    case 'project':
      return `Project ${owner.name}`;
  }
}

export function isSameGitHostConnectionOwner(a: GitHostConnectionOwner, b: GitHostConnectionOwner): boolean {
  return a.type === b.type && (a.type === 'instance' || (b.type !== 'instance' && a.name === b.name));
}
