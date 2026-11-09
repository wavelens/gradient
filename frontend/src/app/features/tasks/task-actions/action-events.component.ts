/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, computed, inject, input, model, ChangeDetectionStrategy } from '@angular/core';
import { toSignal } from '@angular/core/rxjs-interop';
import { groupCatalog } from '@core/models';
import { EventsService } from '@core/services/events.service';
import { ConfigService } from '@core/services/config.service';
import { CheckGroupsComponent, type CheckGroup } from '@shared/ui';

@Component({
  selector: 'app-action-events',
  standalone: true,
  imports: [CheckGroupsComponent],
  template: `<app-check-groups [groups]="grouped()" [(selected)]="selected" [disabled]="disabled()" />`,
  changeDetection: ChangeDetectionStrategy.Eager,
})
export class ActionEventsComponent {
  selected = model.required<string[]>();
  disabled = input(false);
  eventNamePrefixes = input<readonly string[] | undefined>(undefined);

  private readonly catalog = toSignal(inject(EventsService).catalog$, { initialValue: [] });
  private readonly sshEnabled = inject(ConfigService).sshEnabled;

  readonly grouped = computed<CheckGroup[]>(() =>
    groupCatalog(this.catalog(), this.eventNamePrefixes(), this.sshEnabled).map(({ group, items }) => ({ label: group, items })),
  );
}
