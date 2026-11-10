/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, DestroyRef, OnInit, computed, inject, input, output, signal } from '@angular/core';
import { takeUntilDestroyed } from '@angular/core/rxjs-interop';
import { Observable } from 'rxjs';
import { BadgeComponent, ButtonComponent, DialogComponent, IconComponent, LoadStateComponent } from '@gradient/ui/ui';
import {
  DeliveryDetail_TaskActionDeliveryId,
  DeliveryDetail_WebhookDeliveryId,
  DeliveryListItem_TaskActionDeliveryId,
  DeliveryListItem_WebhookDeliveryId,
  eventNameToReadableText,
} from '@core/models';
import { formatDuration, getLastErrorCause, RelativeTimePipe, ServerTimestampToLocalDateTimePipe } from '@shared/text';

export type RecordedOutgoingDeliveryListRow = DeliveryListItem_WebhookDeliveryId | DeliveryListItem_TaskActionDeliveryId;
export type RecordedOutgoingDelivery = DeliveryDetail_WebhookDeliveryId | DeliveryDetail_TaskActionDeliveryId;

export function getDeliveryResultReason(delivery: Pick<RecordedOutgoingDeliveryListRow, 'response_status' | 'error_message'>): string | null {
  if (delivery.response_status != null) return `HTTP ${delivery.response_status}`;
  return delivery.error_message ? getLastErrorCause(delivery.error_message) : null;
}

export function formatBodyAsIndentedJsonIfJson(body: string): string {
  try {
    return JSON.stringify(JSON.parse(body), null, 2);
  } catch {
    return body;
  }
}

const DELIVERIES_PER_PAGE = 50;

@Component({
  selector: 'app-outgoing-webhook-delivery-list-dialog',
  standalone: true,
  imports: [BadgeComponent, ButtonComponent, DialogComponent, IconComponent, LoadStateComponent, RelativeTimePipe, ServerTimestampToLocalDateTimePipe],
  changeDetection: ChangeDetectionStrategy.OnPush,
  templateUrl: './outgoing-webhook-delivery-list-dialog.component.html',
  styleUrl: './outgoing-webhook-delivery-list-dialog.component.scss',
})
export class OutgoingWebhookDeliveryListDialog implements OnInit {
  private destroyRef = inject(DestroyRef);

  webhookOrTaskActionDisplayName = input.required<string>();
  loadDeliveryPage = input.required<(limit: number, offset: number) => Observable<RecordedOutgoingDeliveryListRow[]>>();
  loadDeliveryDetail = input.required<(deliveryId: string) => Observable<RecordedOutgoingDelivery>>();
  closed = output<void>();

  readonly duration = formatDuration;
  readonly eventNameToReadableText = eventNameToReadableText;
  readonly getDeliveryResultReason = getDeliveryResultReason;
  readonly formatBodyAsIndentedJsonIfJson = formatBodyAsIndentedJsonIfJson;
  visible = signal(true);
  deliveries = signal<RecordedOutgoingDeliveryListRow[]>([]);
  loading = signal(true);
  loadError = signal<unknown>(null);
  expandedDeliveryDetailByDeliveryId = signal<ReadonlyMap<string, RecordedOutgoingDelivery | 'loading' | 'error'>>(new Map());
  offset = signal(0);
  hasMore = signal(false);
  pagingSummary = computed(() => `Showing ${this.offset() + 1}–${this.offset() + this.deliveries().length}`);

  ngOnInit(): void {
    this.loadPage();
  }

  loadPage(): void {
    this.loading.set(true);
    this.loadError.set(null);
    this.loadDeliveryPage()(DELIVERIES_PER_PAGE + 1, this.offset())
      .pipe(takeUntilDestroyed(this.destroyRef))
      .subscribe({
        next: (rows) => {
          this.deliveries.set(rows.slice(0, DELIVERIES_PER_PAGE));
          this.hasMore.set(rows.length > DELIVERIES_PER_PAGE);
          this.loading.set(false);
        },
        error: (error: unknown) => {
          this.loadError.set(error);
          this.loading.set(false);
        },
      });
  }

  toggleDeliveryRowExpansion(id: string): void {
    if (this.expandedDeliveryDetailByDeliveryId().has(id)) {
      this.setExpandedDeliveryRowDetailState(id, null);
      return;
    }
    this.setExpandedDeliveryRowDetailState(id, 'loading');
    this.loadDeliveryDetail()(id)
      .pipe(takeUntilDestroyed(this.destroyRef))
      .subscribe({
        next: (detail) => this.expandedDeliveryDetailByDeliveryId().has(id) && this.setExpandedDeliveryRowDetailState(id, detail),
        error: () => this.expandedDeliveryDetailByDeliveryId().has(id) && this.setExpandedDeliveryRowDetailState(id, 'error'),
      });
  }

  changePageOffsetAndLoadPage(direction: 1 | -1): void {
    this.offset.update((offset) => Math.max(0, offset + direction * DELIVERIES_PER_PAGE));
    this.loadPage();
  }

  private setExpandedDeliveryRowDetailState(id: string, detail: RecordedOutgoingDelivery | 'loading' | 'error' | null): void {
    const next = new Map(this.expandedDeliveryDetailByDeliveryId());
    if (detail === null) next.delete(id);
    else next.set(id, detail);
    this.expandedDeliveryDetailByDeliveryId.set(next);
  }
}
