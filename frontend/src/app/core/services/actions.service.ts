/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable, inject } from '@angular/core';
import { Observable } from 'rxjs';
import { ApiService } from './api.service';
import {
  ActionResponse,
  DeletedResponse,
  CreateActionRequest,
  CreateActionResponse,
  UpdateActionRequest,
  DeliveryListItem_TaskActionDeliveryId,
  DeliveryDetail_TaskActionDeliveryId,
} from '@core/models';

@Injectable({ providedIn: 'root' })
export class ActionsService {
  private api = inject(ApiService);

  private base(project: string, proj: string): string {
    return `tasks/${project}/${proj}/actions`;
  }

  list(project: string, proj: string): Observable<ActionResponse[]> {
    return this.api.get<ActionResponse[]>(this.base(project, proj));
  }

  create(project: string, proj: string, body: CreateActionRequest): Observable<CreateActionResponse> {
    return this.api.post<CreateActionResponse>(this.base(project, proj), body);
  }

  read(project: string, proj: string, id: string): Observable<ActionResponse> {
    return this.api.get<ActionResponse>(`${this.base(project, proj)}/${id}`);
  }

  update(project: string, proj: string, id: string, body: UpdateActionRequest): Observable<ActionResponse> {
    return this.api.patch<ActionResponse>(`${this.base(project, proj)}/${id}`, body);
  }

  delete(project: string, proj: string, id: string): Observable<DeletedResponse> {
    return this.api.delete<DeletedResponse>(`${this.base(project, proj)}/${id}`);
  }

  test(project: string, proj: string, id: string): Observable<void> {
    return this.api.post<void>(`${this.base(project, proj)}/${id}/test`);
  }

  regenerateToken(project: string, proj: string, id: string): Observable<{ token: string }> {
    return this.api.post<{ token: string }>(`${this.base(project, proj)}/${id}/regenerate-token`);
  }

  listDeliveries(project: string, proj: string, id: string, limit = 50, offset = 0): Observable<DeliveryListItem_TaskActionDeliveryId[]> {
    return this.api.get<DeliveryListItem_TaskActionDeliveryId[]>(`${this.base(project, proj)}/${id}/deliveries?limit=${limit}&offset=${offset}`);
  }

  getDelivery(project: string, proj: string, actionId: string, deliveryId: string): Observable<DeliveryDetail_TaskActionDeliveryId> {
    return this.api.get<DeliveryDetail_TaskActionDeliveryId>(`${this.base(project, proj)}/${actionId}/deliveries/${deliveryId}`);
  }
}
