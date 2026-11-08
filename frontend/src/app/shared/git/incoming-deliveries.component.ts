/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, OnInit, inject, input, signal } from '@angular/core';
import { BadgeComponent, ButtonComponent, LoadStateComponent } from '@gradient/ui/ui';
import { DeliveryResult, IncomingWebhookDeliveryResponse } from '@core/models';
import { IntegrationsService } from '@core/services/integrations.service';
import { RelativeTimePipe } from '@shared/text';

type Severity = 'success' | 'info' | 'warning' | 'danger';

const SEVERITY: Record<DeliveryResult, Severity> = {
  queued: 'success',
  skipped: 'info',
  no_matching_task: 'warning',
  ignored: 'info',
  refused: 'danger',
};

export function deliverySummary(delivery: IncomingWebhookDeliveryResponse): string {
  switch (delivery.result) {
    case 'queued':
      return delivery.queued === 1 ? 'Queued 1 evaluation' : `Queued ${delivery.queued} evaluations`;
    case 'skipped':
      return delivery.detail ? `Skipped: ${delivery.detail}` : 'Skipped';
    case 'no_matching_task':
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
    <div class="deliveries-head">
      <strong>Recent deliveries</strong>
      <button grButton type="button" label="Refresh" icon="refresh" severity="secondary" size="small"
              [loading]="loading()" [disabled]="loading()" (click)="load()"></button>
    </div>
    <gr-load-state flat [loading]="loading() && deliveries().length === 0" [error]="error()"
                   [empty]="deliveries().length === 0" emptyIcon="inbox"
                   emptyMessage="No deliveries yet. Send a test delivery from the webhook settings of the Git host."
                   errorTitle="Failed to load deliveries" (retry)="load()">
      <ul class="deliveries">
        @for (delivery of deliveries().slice(0, limit()); track delivery.id) {
          <li>
            <gr-badge [severity]="severity(delivery.result)">{{ delivery.event || 'unknown' }}</gr-badge>
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
  private integrations = inject(IntegrationsService);

  project = input.required<string>();
  integrationId = input.required<string>();
  limit = input(10);

  deliveries = signal<IncomingWebhookDeliveryResponse[]>([]);
  loading = signal(false);
  error = signal<unknown>(null);

  ngOnInit(): void {
    this.load();
  }

  load(): void {
    this.loading.set(true);
    this.error.set(null);
    this.integrations.listIntegrationDeliveries(this.project(), this.integrationId()).subscribe({
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

  severity(result: DeliveryResult): Severity {
    return SEVERITY[result];
  }

  summary(delivery: IncomingWebhookDeliveryResponse): string {
    return deliverySummary(delivery);
  }
}
