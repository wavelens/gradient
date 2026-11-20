/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, computed, input, output, signal } from '@angular/core';
import { RouterLink, UrlTree } from '@angular/router';
import { BadgeComponent, type BadgeSeverity, ButtonComponent, CopyFieldComponent, RowComponent, RowListComponent } from '@gradient/ui/ui';
import {
  IncomingWebhookDeliveryResponse,
  TaskCommitStatusPostAttempt,
  TaskGitHostChain,
  TaskGitHostChainStep,
  TaskGitHostChainStepAction,
  TaskGitHostChainStepState,
  eventNameToReadableText,
} from '@core/models';
import { RelativeTimePipe, ServerTimestampToLocalDateTimePipe } from '@shared/text';
import { deliverySummary } from '@shared/git/incoming-deliveries.component';
import { gitHostSettingsLinks } from '@shared/git/git-host';
import { statusPostSummary } from './git-host-connection-health';

type ChainStepKey = keyof TaskGitHostChain;

const STEP_TITLES: Record<ChainStepKey, string> = {
  repository: 'Repository',
  connection: 'Connection',
  incoming_events: 'Incoming events',
  commit_status: 'Commit status',
};

const STATE_BADGES: Record<TaskGitHostChainStepState, { label: string; severity: BadgeSeverity }> = {
  ok: { label: 'Working', severity: 'success' },
  attention: { label: 'Incomplete', severity: 'warning' },
  failing: { label: 'Failing', severity: 'danger' },
  off: { label: 'Off', severity: 'neutral' },
};

const ACTION_LABELS: Record<TaskGitHostChainStepAction['type'], string | null> = {
  edit_repository_url: 'Edit URL',
  select_connection: 'Select connection',
  create_connection: 'New connection',
  install_github_app: null,
  install_webhook: 'Install webhook',
  add_webhook_on_host: 'Open webhook settings',
  turn_on_report_status: 'Turn on',
  test_status_report: 'Test status report',
};

interface ChainStepView extends TaskGitHostChainStep {
  key: ChainStepKey;
  title: string;
  actionLabel: string | null;
}

@Component({
  selector: 'app-task-git-host-chain',
  standalone: true,
  imports: [RouterLink, BadgeComponent, ButtonComponent, CopyFieldComponent, RelativeTimePipe, RowComponent, RowListComponent, ServerTimestampToLocalDateTimePipe],
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    <gr-row-list>
      @for (step of steps(); track step.key) {
        <gr-row [attr.data-testid]="'chain-' + step.key">
          {{ step.title }}
          <gr-badge [severity]="badges[step.state].severity">{{ badges[step.state].label }}</gr-badge>
          <ng-container slot="meta">
            <span data-testid="chain-detail">{{ step.detail }}</span>
            @if (step.at) {
              <span [title]="step.at | serverDate">{{ step.at | relativeTime }}</span>
            }
            @if (step.action?.type === 'add_webhook_on_host' && webhookUrlToAdd(); as url) {
              <gr-copy-field class="chain-step-detail-block" label="Webhook URL" [value]="url" />
            }
            @if (historyShownFor() === step.key) {
              <ul class="chain-step-detail-block chain-step-history" data-testid="chain-history">
                @for (line of historyLines(step.key); track $index) {
                  <li>
                    <gr-badge [severity]="line.severity">{{ line.label }}</gr-badge>
                    <span class="chain-step-history-text">{{ line.text }}</span>
                    <span [title]="line.at | serverDate">{{ line.at | relativeTime }}</span>
                  </li>
                }
              </ul>
            }
          </ng-container>
          <ng-container slot="actions">
            @if (historyLines(step.key).length > 0) {
              <button grButton severity="secondary" size="small" label="History" icon="history"
                [attr.aria-label]="'History of ' + step.title" [attr.aria-expanded]="historyShownFor() === step.key"
                (click)="toggleHistory(step.key)"></button>
            }
            @if (step.action; as stepAction) {
              @if (step.actionLabel; as label) {
                @if (stepAction.type === 'add_webhook_on_host') {
                  @if (webhookSettingsUrl(); as href) {
                    <a grButton severity="secondary" size="small" icon="open_in_new" [label]="label" [href]="href" target="_blank" rel="noopener noreferrer"></a>
                  }
                } @else if (repositoryPageLink(); as link) {
                  <a grButton severity="secondary" size="small" [label]="label" [routerLink]="link" [attr.data-testid]="'chain-action-' + stepAction.type"></a>
                } @else {
                  <button grButton severity="secondary" size="small" [label]="label" [loading]="runningAction() === stepAction.type"
                    [disabled]="!!runningAction()" (click)="action.emit(stepAction)" [attr.data-testid]="'chain-action-' + stepAction.type"></button>
                }
              }
            }
          </ng-container>
        </gr-row>
      }
    </gr-row-list>
  `,
  styles: `
    @use '@gradient/ui/styles/variables' as *;

    :host { display: block; }
    .chain-step-detail-block { flex-basis: 100%; }
    .chain-step-history { list-style: none; margin: 0; padding: 0; }
    .chain-step-history li { display: flex; flex-wrap: wrap; align-items: baseline; gap: $spacing-xs $spacing-sm; padding: $spacing-xs 0; }
    .chain-step-history-text { flex: 1; min-width: 0; overflow-wrap: anywhere; }
  `,
})
export class TaskGitHostChainComponent {
  chain = input.required<TaskGitHostChain>();
  repository = input.required<string>();
  gitHostType = input<string | null>(null);
  deliveries = input<IncomingWebhookDeliveryResponse[]>([]);
  statusPosts = input<TaskCommitStatusPostAttempt[]>([]);
  repositoryPageLink = input<UrlTree | null>(null);
  runningAction = input<TaskGitHostChainStepAction['type'] | null>(null);
  action = output<TaskGitHostChainStepAction>();

  protected readonly badges = STATE_BADGES;
  protected historyShownFor = signal<ChainStepKey | null>(null);

  steps = computed<ChainStepView[]>(() =>
    (Object.keys(STEP_TITLES) as ChainStepKey[]).map((key) => {
      const step = this.chain()[key];
      return { ...step, key, title: STEP_TITLES[key], actionLabel: step.action ? ACTION_LABELS[step.action.type] : null };
    }),
  );

  webhookUrlToAdd = computed(() => {
    const action = this.chain().incoming_events.action;
    return action?.type === 'add_webhook_on_host' ? action.webhook_url : null;
  });

  webhookSettingsUrl = computed(() => gitHostSettingsLinks(this.repository(), this.gitHostType())?.webhooks ?? null);

  private deliveryLines = computed(() =>
    this.deliveries().map((delivery) => ({
      label: delivery.event || 'unknown',
      severity: (delivery.result === 'refused' || delivery.status_code >= 400 ? 'danger' : delivery.result === 'queued' ? 'success' : 'neutral') as BadgeSeverity,
      text: deliverySummary(delivery),
      at: delivery.created_at,
    })),
  );

  private statusPostLines = computed(() =>
    this.statusPosts().map((post) => ({
      label: eventNameToReadableText(post.event),
      severity: (post.success ? 'success' : 'danger') as BadgeSeverity,
      text: statusPostSummary(post),
      at: post.delivered_at,
    })),
  );

  historyLines(key: ChainStepKey) {
    if (key === 'incoming_events') return this.deliveryLines();
    if (key === 'commit_status') return this.statusPostLines();
    return [];
  }

  toggleHistory(key: ChainStepKey): void {
    this.historyShownFor.update((shown) => (shown === key ? null : key));
  }
}
