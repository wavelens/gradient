/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, booleanAttribute, computed, input, output } from '@angular/core';
import { ButtonComponent } from '../button/button.component';
import { EmptyStateComponent } from '../empty-state/empty-state.component';
import { LoadingSpinnerComponent } from '../loading-spinner/loading-spinner.component';

export type LoadPhase = 'loading' | 'error' | 'empty' | 'data';

export class MissingPermissionLoadStateError {
  readonly status = 403;
  constructor(readonly message: string) {}
}

/// The component can treat a rejection (403 or 404) as a final inaccessible state without a retry button. A retry can't change the result.
export function getForbiddenOrMissingKindFromError(error: unknown): 'forbidden' | 'missing' | null {
  const status = (error as { status?: number } | null)?.status;
  if (status === 403) return 'forbidden';
  return status === 404 ? 'missing' : null;
}

function serverReason(error: unknown): string | null {
  const message = (error as { message?: unknown } | null)?.message;
  return typeof message === 'string' && message ? message : null;
}

export function loadErrorText(error: unknown): string {
  if (typeof error === 'string') return error;
  if (error instanceof MissingPermissionLoadStateError) return error.message;
  switch (getForbiddenOrMissingKindFromError(error)) {
    case 'forbidden':
      return 'No permission to view this content. Ask an admin for access.';
    case 'missing': {
      const reason = serverReason(error);
      return `${reason ? `${reason.replace(/\.$/, '')}. ` : ''}The resource doesn't exist or permissions deny access.`;
    }
    default:
      return serverReason(error) ?? 'The server gave no reason. Try again.';
  }
}

const REFUSAL_TITLES = { forbidden: 'No access', missing: 'Not found or access denied' } as const;

/// The component can distinguish loading, failure, empty and loaded states in one place. No failed request can pass for an empty list.
@Component({
  selector: 'gr-load-state',
  standalone: true,
  imports: [ButtonComponent, EmptyStateComponent, LoadingSpinnerComponent],
  template: `
    @switch (phase()) {
      @case ('loading') {
        <gr-loading-spinner [message]="loadingMessage()" />
      }
      @case ('error') {
        <gr-empty-state class="gr-load-state__error" [icon]="refusal() ? 'lock' : 'cloud_off'"
          [title]="refusal() ? refusalTitles[refusal()!] : errorTitle()" [message]="errorText()" [flat]="flat()">
          @if (!refusal()) {
            <button slot="action" grButton label="Try Again" icon="refresh" severity="secondary" (click)="retry.emit()"></button>
          }
        </gr-empty-state>
      }
      @case ('empty') {
        @if (emptyTitle(); as title) {
          <gr-empty-state [icon]="emptyIcon()" [title]="title" [message]="emptyMessage()" [flat]="flat()">
            <ng-container ngProjectAs="[slot=action]"><ng-content select="[slot=empty-action]" /></ng-container>
          </gr-empty-state>
        } @else {
          <ng-content select="[slot=empty]" />
        }
      }
      @default {
        <ng-content />
      }
    }
  `,
  changeDetection: ChangeDetectionStrategy.Eager,
  host: { '[attr.aria-busy]': "phase() === 'loading'" },
})
export class LoadStateComponent {
  loading = input(false, { transform: booleanAttribute });
  error = input<unknown>(null);
  empty = input(false, { transform: booleanAttribute });
  loadingMessage = input('Loading...');
  errorTitle = input('Failed to Load This');
  emptyIcon = input('inbox');
  emptyTitle = input<string>();
  emptyMessage = input<string>();
  flat = input(false, { transform: booleanAttribute });
  retry = output<void>();
  protected readonly refusalTitles = REFUSAL_TITLES;

  phase = computed<LoadPhase>(() => {
    if (this.loading() && this.empty()) return 'loading';
    if (this.error() !== null && this.error() !== undefined) return 'error';
    return this.empty() ? 'empty' : 'data';
  });
  errorText = computed(() => loadErrorText(this.error()));
  refusal = computed(() => getForbiddenOrMissingKindFromError(this.error()));
}
