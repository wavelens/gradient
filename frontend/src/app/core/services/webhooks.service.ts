/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable, inject } from '@angular/core';
import { Observable } from 'rxjs';
import { ApiService } from './api.service';
import {
  CreateWebhookRequest,
  CreateWebhookResponse,
  UpdateWebhookRequest,
  Webhook,
  WebhookDelivery,
  WebhookDeliveryDetail,
  WebhookScopeRef,
  webhooksBase,
} from '@core/models';

@Injectable({ providedIn: 'root' })
export class WebhooksService {
  private api = inject(ApiService);

  list(scope: WebhookScopeRef): Observable<Webhook[]> {
    return this.api.get<Webhook[]>(webhooksBase(scope));
  }

  create(scope: WebhookScopeRef, body: CreateWebhookRequest): Observable<CreateWebhookResponse> {
    return this.api.post<CreateWebhookResponse>(webhooksBase(scope), body);
  }

  update(scope: WebhookScopeRef, id: string, body: UpdateWebhookRequest): Observable<Webhook> {
    return this.api.patch<Webhook>(`${webhooksBase(scope)}/${id}`, body);
  }

  delete(scope: WebhookScopeRef, id: string): Observable<{ deleted: boolean }> {
    return this.api.delete<{ deleted: boolean }>(`${webhooksBase(scope)}/${id}`);
  }

  test(scope: WebhookScopeRef, id: string): Observable<WebhookDelivery> {
    return this.api.post<WebhookDelivery>(`${webhooksBase(scope)}/${id}/test`);
  }

  rotateSecret(scope: WebhookScopeRef, id: string): Observable<{ secret: string }> {
    return this.api.post<{ secret: string }>(`${webhooksBase(scope)}/${id}/rotate-secret`);
  }

  deliveries(scope: WebhookScopeRef, id: string, limit = 50, offset = 0): Observable<WebhookDelivery[]> {
    return this.api.get<WebhookDelivery[]>(
      `${webhooksBase(scope)}/${id}/deliveries?limit=${limit}&offset=${offset}`,
    );
  }

  delivery(scope: WebhookScopeRef, id: string, deliveryId: string): Observable<WebhookDeliveryDetail> {
    return this.api.get<WebhookDeliveryDetail>(`${webhooksBase(scope)}/${id}/deliveries/${deliveryId}`);
  }
}
