/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, computed, input } from '@angular/core';
import { RouterLink, UrlTree } from '@angular/router';
import { ButtonComponent, RowComponent, RowListComponent } from '@gradient/ui/ui';
import { TaskGitHostChain, TaskGitHostChainStep, TaskGitHostChainStepAction } from '@core/models';
import { RelativeTimePipe, ServerTimestampToLocalDateTimePipe } from '@shared/text';
import { ChainStepStateBadgeComponent } from './chain-step-state-badge.component';

export type ChainStepKey = keyof TaskGitHostChain;

export const CHAIN_STEP_TITLES: Record<ChainStepKey, string> = {
  repository: 'Repository',
  connection: 'Connection',
  incoming_events: 'Incoming events',
  commit_status: 'Commit status',
};

const ACTION_LABELS: Record<TaskGitHostChainStepAction['type'], string | null> = {
  edit_repository_url: 'Edit URL',
  select_connection: 'Select connection',
  create_connection: 'New connection',
  install_github_app: null,
  install_webhook: 'Install webhook',
  add_webhook_on_host: 'Add webhook',
  turn_on_report_status: 'Turn on',
  test_status_report: 'Test status report',
};

export function nextChainStepKey(chain: TaskGitHostChain): ChainStepKey | undefined {
  return (Object.keys(CHAIN_STEP_TITLES) as ChainStepKey[]).find((key) => chain[key].action);
}

interface ChainStepView extends TaskGitHostChainStep {
  key: ChainStepKey;
  title: string;
  actionLabel: string | null;
}

@Component({
  selector: 'app-task-git-host-chain',
  standalone: true,
  imports: [RouterLink, ButtonComponent, ChainStepStateBadgeComponent, RelativeTimePipe, RowComponent, RowListComponent, ServerTimestampToLocalDateTimePipe],
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    <gr-row-list>
      @for (step of steps(); track step.key) {
        <gr-row [attr.data-testid]="'chain-' + step.key">
          {{ step.title }}
          <app-chain-step-state-badge [state]="step.state" />
          <ng-container slot="meta">
            <span data-testid="chain-detail">{{ step.detail }}</span>
            @if (step.at) {
              <span [title]="step.at | serverDate">{{ step.at | relativeTime }}</span>
            }
          </ng-container>
          <ng-container slot="actions">
            @if (step.action && step.actionLabel) {
              <a grButton severity="secondary" size="small" [label]="step.actionLabel" [routerLink]="repositoryPageLink()"
                [attr.data-testid]="'chain-action-' + step.action.type"></a>
            }
          </ng-container>
        </gr-row>
      }
    </gr-row-list>
  `,
  styles: `:host { display: block; }`,
})
export class TaskGitHostChainComponent {
  chain = input.required<TaskGitHostChain>();
  repositoryPageLink = input.required<UrlTree>();

  steps = computed<ChainStepView[]>(() => {
    const nextKey = nextChainStepKey(this.chain());
    return (Object.keys(CHAIN_STEP_TITLES) as ChainStepKey[]).map((key) => {
      const step = this.chain()[key];
      const action = key === nextKey ? step.action : null;
      return { ...step, key, action, title: CHAIN_STEP_TITLES[key], actionLabel: action ? ACTION_LABELS[action.type] : null };
    });
  });
}
