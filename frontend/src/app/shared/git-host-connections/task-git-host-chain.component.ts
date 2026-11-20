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
import { ChainStepStateBadgeComponent } from './chain-step-state-badge.component';

export type ChainStepKey = keyof TaskGitHostChain;

export const CHAIN_STEP_TITLES: Record<ChainStepKey, string> = {
  repository: 'Repository',
  connection: 'Connection',
  incoming_events: 'Incoming events',
  commit_status: 'Commit status',
};

const STATES_WORST_FIRST: TaskGitHostChainStepState[] = ['failing', 'attention', 'off', 'ok'];

export function nextChainStepKey(chain: TaskGitHostChain): ChainStepKey | undefined {
  return (Object.keys(CHAIN_STEP_TITLES) as ChainStepKey[]).find((key) => chain[key].action);
}

@Component({
  selector: 'app-task-git-host-chain',
  standalone: true,
  imports: [ChainStepStateBadgeComponent],
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    <p class="chain-summary" data-testid="chain-summary">
      <app-chain-step-state-badge [state]="worstStep().state" />
      <span class="chain-summary__text"><strong>{{ worstStep().title }}</strong> {{ worstStep().detail }}</span>
      <a class="chain-summary__link" [href]="repositoryPageHref()" (click)="openRepositoryPage($event)" data-testid="open-repository">Open repository</a>
    </p>
  `,
  styles: `
    @use '@gradient/ui/styles/variables' as *;

    :host { display: block; }
    .chain-summary { display: flex; flex-wrap: wrap; align-items: center; gap: $spacing-sm; margin: 0; }
    .chain-summary__text { flex: 1 1 12rem; min-width: 0; overflow-wrap: anywhere; }
    .chain-summary__link { text-decoration: underline; white-space: nowrap; }
  `,
})
export class TaskGitHostChainComponent {
  private router = inject(Router);
  private locationStrategy = inject(LocationStrategy);

  chain = input.required<TaskGitHostChain>();
  repositoryPageLink = input.required<UrlTree>();
  dialogToCloseFirst = input<Pick<DialogComponent, 'close'> | null>(null);

  worstStep = computed(() => {
    const keys = Object.keys(CHAIN_STEP_TITLES) as ChainStepKey[];
    const rank = (key: ChainStepKey) => STATES_WORST_FIRST.indexOf(this.chain()[key].state);
    const key = keys.reduce((worst, candidate) => (rank(candidate) < rank(worst) ? candidate : worst));
    return { ...this.chain()[key], title: CHAIN_STEP_TITLES[key] };
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
