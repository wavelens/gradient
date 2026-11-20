/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, computed, inject, input } from '@angular/core';
import { LocationStrategy } from '@angular/common';
import { Router, UrlTree } from '@angular/router';
import type { DialogComponent } from '@gradient/ui/ui';
import { TaskGitHostChain, TaskGitHostChainStepState } from '@core/models';
import { TaskRepositoryStatusCheckStepStateBadgeComponent } from './task-repository-status-check-step-state-badge.component';

export type TaskRepositoryStatusCheckStepKey = keyof TaskGitHostChain;

export const taskRepositoryStatusCheckStepTitles: Record<TaskRepositoryStatusCheckStepKey, string> = {
  repository: 'Repository',
  connection: 'Connection',
  incoming_events: 'Incoming Events',
  commit_status: 'Commit Status',
};

const STATES_WORST_FIRST: TaskGitHostChainStepState[] = ['failing', 'attention', 'off', 'ok'];

export function getFirstTaskRepositoryStatusCheckStepWithSuggestedFixKey(chain: TaskGitHostChain): TaskRepositoryStatusCheckStepKey | undefined {
  return (Object.keys(taskRepositoryStatusCheckStepTitles) as TaskRepositoryStatusCheckStepKey[]).find((key) => chain[key].action);
}

@Component({
  selector: 'app-task-repository-status-check-summary',
  standalone: true,
  imports: [TaskRepositoryStatusCheckStepStateBadgeComponent],
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    <p class="status-check-summary" data-testid="status-check-summary">
      <app-task-repository-status-check-step-state-badge [state]="worstStep().state" />
      <span class="status-check-summary__text"><strong>{{ worstStep().title }}</strong> {{ worstStep().detail }}</span>
      <a class="status-check-summary__link" [href]="repositoryPageHref()" (click)="openRepositoryPage($event)" data-testid="open-repository">Open repository</a>
    </p>
  `,
  styles: `
    @use '@gradient/ui/styles/variables' as *;

    :host { display: block; }
    .status-check-summary { display: flex; flex-wrap: wrap; align-items: center; gap: $spacing-sm; margin: 0; }
    .status-check-summary__text { flex: 1 1 12rem; min-width: 0; overflow-wrap: anywhere; }
    .status-check-summary__link { text-decoration: underline; white-space: nowrap; }
  `,
})
export class TaskRepositoryStatusCheckSummaryComponent {
  private router = inject(Router);
  private locationStrategy = inject(LocationStrategy);

  chain = input.required<TaskGitHostChain>();
  repositoryPageLink = input.required<UrlTree>();
  dialogToCloseFirst = input<Pick<DialogComponent, 'close'> | null>(null);

  worstStep = computed(() => {
    const keys = Object.keys(taskRepositoryStatusCheckStepTitles) as TaskRepositoryStatusCheckStepKey[];
    const rank = (key: TaskRepositoryStatusCheckStepKey) => STATES_WORST_FIRST.indexOf(this.chain()[key].state);
    const key = keys.reduce((worst, candidate) => (rank(candidate) < rank(worst) ? candidate : worst));
    return { ...this.chain()[key], title: taskRepositoryStatusCheckStepTitles[key] };
  });
  repositoryPageHref = computed(() => this.locationStrategy.prepareExternalUrl(this.router.serializeUrl(this.repositoryPageLink())));

  openRepositoryPage(event: MouseEvent): void {
    if (event.button !== 0 || event.ctrlKey || event.metaKey || event.shiftKey || event.altKey) return;
    event.preventDefault();
    const open = () => void this.router.navigateByUrl(this.repositoryPageLink());
    const dialog = this.dialogToCloseFirst();
    if (dialog) dialog.close(open);
    else open();
  }
}
