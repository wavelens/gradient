/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable, inject } from '@angular/core';
import { Observable } from 'rxjs';
import { ApiService } from './api.service';
import {
  Action,
  ActionDelivery,
  ActionDeliveryDetail,
  CreateActionRequest,
  CreateActionResponse,
  UpdateActionRequest,
} from '@core/models';

@Injectable({ providedIn: 'root' })
export class ActionsService {
  private api = inject(ApiService);

  private base(org: string, proj: string): string {
    return `projects/${org}/${proj}/actions`;
  }

  list(org: string, proj: string): Observable<Action[]> {
    return this.api.get<Action[]>(this.base(org, proj));
  }

  create(org: string, proj: string, body: CreateActionRequest): Observable<CreateActionResponse> {
    return this.api.post<CreateActionResponse>(this.base(org, proj), body);
  }

  read(org: string, proj: string, id: string): Observable<Action> {
    return this.api.get<Action>(`${this.base(org, proj)}/${id}`);
  }

  update(org: string, proj: string, id: string, body: UpdateActionRequest): Observable<Action> {
    return this.api.patch<Action>(`${this.base(org, proj)}/${id}`, body);
  }

  delete(org: string, proj: string, id: string): Observable<{ deleted: boolean }> {
    return this.api.delete<{ deleted: boolean }>(`${this.base(org, proj)}/${id}`);
  }

  test(org: string, proj: string, id: string): Observable<void> {
    return this.api.post<void>(`${this.base(org, proj)}/${id}/test`);
  }

  regenerateToken(org: string, proj: string, id: string): Observable<{ token: string }> {
    return this.api.post<{ token: string }>(`${this.base(org, proj)}/${id}/regenerate-token`);
  }

  listDeliveries(org: string, proj: string, id: string, limit = 50, offset = 0): Observable<ActionDelivery[]> {
    return this.api.get<ActionDelivery[]>(`${this.base(org, proj)}/${id}/deliveries?limit=${limit}&offset=${offset}`);
  }

  getDelivery(org: string, proj: string, actionId: string, deliveryId: string): Observable<ActionDeliveryDetail> {
    return this.api.get<ActionDeliveryDetail>(`${this.base(org, proj)}/${actionId}/deliveries/${deliveryId}`);
  }
}
