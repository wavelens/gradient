/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

export type WebhookScopeRef =
  | { kind: 'project'; name: string }
  | { kind: 'cache'; name: string }
  | { kind: 'instance' };

export interface Webhook {
  id: string;
  scope: 'project' | 'cache' | 'instance';
  name: string;
  url: string;
  events: string[];
  active: boolean;
  last_fired_at: string | null;
  created_by: string;
  created_at: string;
  updated_at: string;
}

export interface CreateWebhookRequest {
  name: string;
  url: string;
  events: string[];
  active?: boolean;
}

export type UpdateWebhookRequest = Partial<CreateWebhookRequest>;

export interface CreateWebhookResponse {
  webhook: Webhook;
  secret: string;
}

export interface WebhookDelivery {
  id: string;
  event: string;
  success: boolean;
  response_status: number | null;
  error_message: string | null;
  duration_ms: number;
  delivered_at: string;
}

export interface WebhookDeliveryDetail extends WebhookDelivery {
  request_body: string;
  response_body: string | null;
}

export function webhooksBase(scope: WebhookScopeRef): string {
  switch (scope.kind) {
    case 'project':
      return `projects/${scope.name}/webhooks`;
    case 'cache':
      return `caches/${scope.name}/webhooks`;
    case 'instance':
      return 'admin/webhooks';
  }
}
