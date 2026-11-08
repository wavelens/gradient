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

/** Events that the server can deliver to notifications for each scope. Events belong to a project, cache, or team. Those owners route their events. Instance notifications receive all events, including personal events such as login. */
export const eventNamePrefixesByNotificationScope: Record<Exclude<WebhookScopeRef['kind'], 'instance'>, readonly string[]> = {
  project: ['build', 'evaluation', 'exec_job', 'project', 'task', 'cache.subscription', 'team.grant', 'team.request'],
  cache: ['cache', 'team.grant', 'team.request'],
  team: ['team'],
};

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
