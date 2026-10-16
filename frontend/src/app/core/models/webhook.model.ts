/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

export type WebhookScopeRef =
  | { kind: 'project'; name: string }
  | { kind: 'cache'; name: string }
  | { kind: 'team'; name: string }
  | { kind: 'instance' };

export function webhooksBase(scope: WebhookScopeRef): string {
  switch (scope.kind) {
    case 'project':
      return `projects/${scope.name}/webhooks`;
    case 'cache':
      return `caches/${scope.name}/webhooks`;
    case 'team':
      return `teams/${scope.name}/webhooks`;
    case 'instance':
      return 'admin/webhooks';
  }
}
