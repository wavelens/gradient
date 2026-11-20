/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, OnInit, inject, input, signal } from '@angular/core';
import { BadgeComponent, type BadgeSeverity, ButtonComponent, LoadStateComponent } from '@gradient/ui/ui';
import { DeliveryResult, GitHostConnectionOwner, IncomingWebhookDeliveryResponse } from '@core/models';
import { GitHostConnectionsApi } from '@core/services/git-host-connections.service';
import { RelativeTimePipe } from '@shared/text';

const SEVERITY: Record<DeliveryResult, BadgeSeverity> = {
  queued: 'success',
  skipped: 'info',
  no_matching_task: 'warning',
  ignored: 'info',
  refused: 'danger',
};

export function deliveryBadgeSeverity(delivery: IncomingWebhookDeliveryResponse): BadgeSeverity {
  return delivery.status_code >= 400 ? 'danger' : SEVERITY[delivery.result];
}

export function deliverySummary(delivery: IncomingWebhookDeliveryResponse): string {
  switch (delivery.result) {
    case 'queued':
      return delivery.queued === 1 ? 'Queued 1 evaluation' : `Queued ${delivery.queued} evaluations`;
    case 'skipped':
      return delivery.detail ? `Skipped: ${delivery.detail}` : 'Skipped';
    case 'no_matching_task':
      if (delivery.detail) return delivery.detail;
      return delivery.repository ? `No task tracks ${delivery.repository}` : 'No task tracks this repository';
    case 'ignored':
      return `Received ${delivery.event || 'unknown'} event`;
    case 'refused': {
      const refused = `Refused with HTTP ${delivery.status_code}`;
      return delivery.detail ? `${refused}: ${delivery.detail}` : refused;
    }
  }
}

@Component({
  selector: 'app-incoming-deliveries',
  standalone: true,
  imports: [BadgeComponent, ButtonComponent, LoadStateComponent, RelativeTimePipe],
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    @if (!error() && (deliveries().length > 0 || refusedSignature() || !loading())) {
      <div class="deliveries-head">
        @if (deliveries().length > 0 || refusedSignature()) {
          <strong>Recent deliveries</strong>
        } @else {
          <span class="text-secondary">No deliveries yet. Send a test delivery from the webhook settings of the Git host.</span>
        }
        <button grButton type="button" label="Refresh" icon="refresh" severity="secondary" size="small"
                [loading]="loading()" [disabled]="loading()" (click)="load()"></button>
      </div>
    }
    <gr-load-state flat [loading]="loading() && deliveries().length === 0" [error]="error()"
                   [empty]="deliveries().length === 0 && !refusedSignature()" errorTitle="Failed to load deliveries" (retry)="load()">
      <ul class="deliveries">
        @if (refusedSignature(); as refused) {
          <li data-testid="refused-signature">
            <gr-badge severity="danger">signature</gr-badge>
            <span class="summary">{{ refused.count === 1 ? 'Refused 1 delivery' : 'Refused ' + refused.count + ' deliveries' }} with a wrong signature</span>
            <span class="text-secondary when">{{ refused.at | relativeTime }}</span>
          </li>
        }
        @for (delivery of deliveries().slice(0, limit()); track delivery.id) {
          <li>
            <gr-badge [severity]="severity(delivery)">{{ delivery.event || 'unknown' }}</gr-badge>
            <span class="summary">{{ summary(delivery) }}</span>
            <span class="text-secondary when">{{ delivery.created_at | relativeTime }}</span>
          </li>
        }
      </ul>
    </gr-load-state>
  `,
  styles: `
    @use '@gradient/ui/styles/variables' as *;

    :host { display: block; }
    .deliveries-head { display: flex; align-items: center; justify-content: space-between; gap: $spacing-sm; }
    .deliveries { list-style: none; margin: 0; padding: 0; }
    .deliveries li { display: flex; align-items: baseline; gap: $spacing-sm; padding: $spacing-xs 0; }
    .summary { flex: 1; word-break: break-word; }
    .when { white-space: nowrap; }
  `,
})
export class IncomingDeliveriesComponent implements OnInit {
  private connections = inject(GitHostConnectionsApi);

  gitHostConnectionOwner = input.required<GitHostConnectionOwner>();
  connectionId = input.required<string>();
  limit = input(10);
  refusedSignature = input<{ at: string; count: number } | null>(null);

  deliveries = signal<IncomingWebhookDeliveryResponse[]>([]);
  loading = signal(false);
  error = signal<unknown>(null);

  ngOnInit(): void {
    this.load();
  }

  load(): void {
    this.loading.set(true);
    this.error.set(null);
    this.connections.listDeliveries(this.gitHostConnectionOwner(), this.connectionId()).subscribe({
      next: (list) => {
        this.deliveries.set(list);
        this.loading.set(false);
      },
      error: (error: unknown) => {
        this.error.set(error);
        this.loading.set(false);
      },
    });
  }

  severity(delivery: IncomingWebhookDeliveryResponse): BadgeSeverity {
    return deliveryBadgeSeverity(delivery);
  }

  summary(delivery: IncomingWebhookDeliveryResponse): string {
    return deliverySummary(delivery);
  }
}
