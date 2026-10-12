/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable, inject } from '@angular/core';
import { Observable } from 'rxjs';
import { ApiService } from './api.service';
import {
  WebhookScopeRef,
  webhooksBase,
  DeletedResponse,
  RotatedSecret,
  WebhookResponse,
  CreateWebhookRequest,
  CreateWebhookResponse,
  UpdateWebhookRequest,
  DeliveryListItem_WebhookDeliveryId,
  DeliveryDetail_WebhookDeliveryId,
} from '@core/models';

@Injectable({ providedIn: 'root' })
export class WebhooksService {
  private api = inject(ApiService);

  list(scope: WebhookScopeRef): Observable<WebhookResponse[]> {
    return this.api.get<WebhookResponse[]>(webhooksBase(scope));
  }

  create(scope: WebhookScopeRef, body: CreateWebhookRequest): Observable<CreateWebhookResponse> {
    return this.api.post<CreateWebhookResponse>(webhooksBase(scope), body);
  }

  update(scope: WebhookScopeRef, id: string, body: UpdateWebhookRequest): Observable<WebhookResponse> {
    return this.api.patch<WebhookResponse>(`${webhooksBase(scope)}/${id}`, body);
  }

  delete(scope: WebhookScopeRef, id: string): Observable<DeletedResponse> {
    return this.api.delete<DeletedResponse>(`${webhooksBase(scope)}/${id}`);
  }

  test(scope: WebhookScopeRef, id: string): Observable<DeliveryListItem_WebhookDeliveryId> {
    return this.api.post<DeliveryListItem_WebhookDeliveryId>(`${webhooksBase(scope)}/${id}/test`);
  }

  rotateSecret(scope: WebhookScopeRef, id: string): Observable<RotatedSecret> {
    return this.api.post<RotatedSecret>(`${webhooksBase(scope)}/${id}/rotate-secret`);
  }

  deliveries(scope: WebhookScopeRef, id: string, limit = 50, offset = 0): Observable<DeliveryListItem_WebhookDeliveryId[]> {
    return this.api.get<DeliveryListItem_WebhookDeliveryId[]>(
      `${webhooksBase(scope)}/${id}/deliveries?limit=${limit}&offset=${offset}`,
    );
  }

  delivery(scope: WebhookScopeRef, id: string, deliveryId: string): Observable<DeliveryDetail_WebhookDeliveryId> {
    return this.api.get<DeliveryDetail_WebhookDeliveryId>(`${webhooksBase(scope)}/${id}/deliveries/${deliveryId}`);
  }
}
