/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, input } from '@angular/core';
import { Observable } from 'rxjs';
import { BadgeComponent, TooltipDirective } from '@gradient/ui/ui';
import { ConfirmService } from '@shared/confirm';

const drainingBadgeWorkerDrainedItselfTooltip =
  'The worker started this drain on its own, for example during shutdown. Worker restarts remove the drain.';

export function confirmAndDrainWorker<T>(confirm: ConfirmService, worker: string, drain: () => Observable<T>): Observable<T> {
  return confirm.confirm({
    title: 'Worker Drain',
    target: worker,
    consequence: 'The worker can finish running jobs. New jobs wait for Undrain.',
    confirmLabel: 'Drain',
    failure: 'Failed to drain workers',
    action: drain,
  });
}

@Component({
  selector: 'app-worker-drain-badge',
  standalone: true,
  imports: [BadgeComponent, TooltipDirective],
  template: `
    @if (requestedByAdmin()) {
      <gr-badge severity="warning">Draining</gr-badge>
    } @else if (draining()) {
      <gr-badge severity="warning" tabindex="0" [grTooltip]="selfDrainTooltip">Draining</gr-badge>
    }
  `,
  changeDetection: ChangeDetectionStrategy.Eager,
})
export class WorkerDrainingBadgeComponent {
  draining = input(false);
  requestedByAdmin = input(false);

  protected readonly selfDrainTooltip = drainingBadgeWorkerDrainedItselfTooltip;
}
