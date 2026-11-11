/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, computed, effect, inject, input, output, signal, untracked } from '@angular/core';
import { FormsModule } from '@angular/forms';
import { ButtonComponent, DialogComponent, SelectButtonComponent } from '@gradient/ui/ui';
import { apiErrorMessage } from '@core/services/api.service';
import { CachesService } from '@core/services/caches.service';
import { NotifyService } from '@core/services/notify.service';
import { NarSummary } from '@core/models';

@Component({
  selector: 'app-cache-nars-pin-scope-dialog',
  standalone: true,
  imports: [FormsModule, ButtonComponent, DialogComponent, SelectButtonComponent],
  template: `
    @let current = row();
    <gr-dialog
      [header]="unpinning() ? 'Unpin package' : 'Pin package'"
      [visible]="current !== null"
      (visibleChange)="$event || cancel()"
      [busy]="running()"
      [error]="error()"
      width="480px"
    >
      @if (current) {
        <p><strong>{{ current.package }} ({{ current.hash }})</strong></p>
        @if (unpinning()) {
          <p>Garbage collection can remove unpinned packages from this cache again once those packages are no longer in use.</p>
        } @else {
          <p>Pinned packages stay in this cache through garbage collection. They remain there no matter how long ago the last fetch was and even after the evaluations that built them are gone.</p>
        }
        <gr-select-button
          ariaLabel="Pin scope"
          [options]="scopes"
          optionLabel="label"
          optionValue="value"
          [allowEmpty]="false"
          [ngModel]="includeReferencedPaths()"
          (ngModelChange)="includeReferencedPaths.set($event)"
          data-testid="pin-scope"
        />
        @if (includeReferencedPaths()) {
          <p class="text-secondary">
            @if (unpinning()) {
              Also unpin all paths with a direct or indirect reference from this package. This includes paths that were pinned on their own.
            } @else {
              Also pin all paths in this cache that are referenced directly or indirectly by the package. Use this option for release artifacts that need to stay installable.
            }
          </p>
        }
      }
      <div grDialogFooter>
        <button grButton label="Cancel" severity="secondary" [disabled]="running()" (click)="cancel()"></button>
        <button grButton [label]="unpinning() ? 'Unpin' : 'Pin'" severity="primary" [loading]="running()" (click)="run()"></button>
      </div>
    </gr-dialog>
  `,
  changeDetection: ChangeDetectionStrategy.Eager,
})
export class CacheNarsPinScopeDialogComponent {
  private cachesService = inject(CachesService);
  private notify = inject(NotifyService);

  cacheName = input.required<string>();
  row = input<NarSummary | null>(null);
  changed = output<void>();
  closed = output<void>();

  protected readonly scopes = [
    { label: 'Only this path', value: false },
    { label: 'With all referenced paths', value: true },
  ];
  protected unpinning = computed(() => !!this.row()?.pinned_at);
  protected includeReferencedPaths = signal(false);
  protected running = signal(false);
  protected error = signal<string | null>(null);

  constructor() {
    effect(() => {
      this.row();
      untracked(() => {
        this.includeReferencedPaths.set(false);
        this.error.set(null);
      });
    });
  }

  protected cancel(): void {
    if (!this.running()) this.closed.emit();
  }

  protected run(): void {
    const row = this.row();
    if (!row || this.running()) return;
    const unpinning = this.unpinning();
    const request = unpinning
      ? this.cachesService.unpinCacheNar(this.cacheName(), row.hash, this.includeReferencedPaths())
      : this.cachesService.pinCacheNar(this.cacheName(), row.hash, this.includeReferencedPaths());
    this.running.set(true);
    this.error.set(null);
    request.subscribe({
      next: ({ pin_state_changed_path_count: count }) => {
        this.running.set(false);
        this.notify.success(`${unpinning ? 'Unpinned' : 'Pinned'} ${count} ${count === 1 ? 'path' : 'paths'}`);
        this.changed.emit();
        this.closed.emit();
      },
      error: (error: unknown) => {
        this.running.set(false);
        this.error.set(apiErrorMessage(error, unpinning ? 'Failed to unpin package.' : 'Failed to pin package.'));
      },
    });
  }
}
