/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, computed, inject, input, output, signal, viewChild } from '@angular/core';
import { ButtonComponent, CopyFieldComponent, SettingsSectionComponent } from '@gradient/ui/ui';
import { TaskRepositoryResponse } from '@core/models';
import { TasksService } from '@core/services/tasks.service';
import { NotifyService } from '@core/services/notify.service';
import { deliveryBadgeSeverity, deliverySummary, gitHostSettingsLinks } from '@shared/git';
import { RelativeTimePipe, ServerTimestampToLocalDateTimePipe } from '@shared/text';
import { ChainStepStateBadgeComponent } from '@shared/git-host-connections/chain-step-state-badge.component';
import { GitHostConnectionDialogComponent } from '@shared/git-host-connections/git-host-connection-dialog.component';
import { StepHistoryLine, StepHistoryListComponent } from './step-history-list.component';

const EARLIER_DELIVERIES_SHOWN = 5;

@Component({
  selector: 'app-repository-incoming-events',
  standalone: true,
  imports: [ButtonComponent, ChainStepStateBadgeComponent, CopyFieldComponent, GitHostConnectionDialogComponent, RelativeTimePipe, ServerTimestampToLocalDateTimePipe, SettingsSectionComponent, StepHistoryListComponent],
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    <gr-settings-section title="Incoming events">
      <app-chain-step-state-badge slot="after-title" [state]="step().state" />
      @if (!applies()) {
        <span class="text-secondary">{{ step().detail }}</span>
      } @else {
        <div class="step-status-row">
          <p class="step-status" data-testid="step-status">
            <span>{{ step().detail }}</span>
            @if (step().at; as at) {
              <span class="text-secondary" [title]="at | serverDate">{{ at | relativeTime }}</span>
            }
          </p>
          @if (step().action?.type === 'install_webhook') {
            <button grButton label="Install webhook" icon="webhook" [severity]="hasPrimaryAction() ? 'primary' : 'secondary'"
              [loading]="installing()" [disabled]="installing()" (click)="installWebhook()" data-testid="install-webhook"></button>
          }
          @if (step().action?.type === 'change_webhook_secret' && state().git_host_connection; as connection) {
            <button grButton label="Change webhook secret" icon="key" [severity]="hasPrimaryAction() ? 'primary' : 'secondary'"
              (click)="connectionDialog()?.openEdit(connection, { focusWebhookSecret: true })" data-testid="change-webhook-secret"></button>
          }
        </div>
        @if (webhookUrlToAdd(); as url) {
          <div class="webhook-by-hand">
            <gr-copy-field label="Webhook URL" [value]="url" />
            @if (webhookSettingsUrl(); as href) {
              <a grButton icon="open_in_new" label="Open webhook settings" [severity]="hasPrimaryAction() ? 'primary' : 'secondary'"
                [href]="href" target="_blank" rel="noopener noreferrer"></a>
            }
          </div>
        }
        @if (earlierDeliveries().length > 0) {
          <app-step-history-list [lines]="earlierDeliveries()" />
        }
      }
    </gr-settings-section>
    @if (state().git_host_connection; as connection) {
      <app-git-host-connection-dialog [owner]="connection.owner" [repository]="state().repository" (saved)="changed.emit()" />
    }
  `,
  styles: `
    @use '@gradient/ui/styles/variables' as *;

    .step-status-row { display: flex; flex-wrap: wrap; align-items: center; justify-content: space-between; gap: $spacing-sm $spacing-md; }
    .step-status { display: flex; flex-wrap: wrap; gap: $spacing-sm; flex: 1 1 16rem; min-width: 0; margin: 0; overflow-wrap: anywhere; }
    .webhook-by-hand { display: flex; flex-wrap: wrap; align-items: center; gap: $spacing-sm; margin-top: $spacing-md; }
    .webhook-by-hand > gr-copy-field { flex: 1 1 20rem; min-width: 0; }
    app-step-history-list { margin-top: $spacing-md; }
  `,
})
export class RepositoryIncomingEventsComponent {
  private tasks = inject(TasksService);
  private notify = inject(NotifyService);

  project = input.required<string>();
  task = input.required<string>();
  state = input.required<TaskRepositoryResponse>();
  hasPrimaryAction = input(false);
  changed = output<void>();

  installing = signal(false);
  connectionDialog = viewChild(GitHostConnectionDialogComponent);

  step = computed(() => this.state().chain.incoming_events);
  applies = computed(() => this.state().chain.connection.state === 'ok' && !!this.state().git_host_connection?.receives_events);
  webhookUrlToAdd = computed(() => {
    const action = this.step().action;
    return action?.type === 'add_webhook_on_host' ? action.webhook_url : null;
  });
  webhookSettingsUrl = computed(
    () => gitHostSettingsLinks(this.state().repository, this.state().git_host_connection?.git_host_type ?? null)?.webhooks ?? null,
  );
  earlierDeliveries = computed<StepHistoryLine[]>(() => {
    const deliveries = this.state().recent_deliveries ?? [];
    const earlier = deliveries[0]?.created_at === this.step().at ? deliveries.slice(1) : deliveries;
    return earlier.slice(0, EARLIER_DELIVERIES_SHOWN).map((delivery) => ({
      id: delivery.id,
      label: delivery.event || 'unknown',
      severity: deliveryBadgeSeverity(delivery),
      text: deliverySummary(delivery),
      at: delivery.created_at,
    }));
  });

  installWebhook(): void {
    if (this.installing()) return;
    this.installing.set(true);
    this.tasks.installWebhook(this.project(), this.task()).subscribe({
      next: () => {
        this.installing.set(false);
        this.notify.success('Webhook installed');
        this.changed.emit();
      },
      error: (error: unknown) => {
        this.installing.set(false);
        this.notify.failure("Couldn't install webhook", error);
      },
    });
  }
}
