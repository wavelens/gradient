/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, computed, inject, input, output, ChangeDetectionStrategy } from '@angular/core';
import { toSignal } from '@angular/core/rxjs-interop';
import { groupCatalog } from '@core/models';
import { EventsService } from '@core/services/events.service';
import { CheckGroupsComponent, type CheckGroup } from '@shared/ui';

@Component({
  selector: 'app-action-events',
  standalone: true,
  imports: [CheckGroupsComponent],
  template: `<app-check-groups [groups]="grouped()" [selected]="selected()" [disabled]="disabled()" (selectedChange)="selectedChange.emit($event)" />`,
  changeDetection: ChangeDetectionStrategy.Eager,
})
export class ActionEventsComponent {
  selected = input.required<string[]>();
  disabled = input(false);
  families = input<string[] | undefined>(undefined);
  selectedChange = output<string[]>();

  private readonly catalog = toSignal(inject(EventsService).catalog$, { initialValue: [] });

  readonly grouped = computed<CheckGroup[]>(() =>
    groupCatalog(this.catalog(), this.families()).map(({ group, items }) => ({ label: group, items })),
  );
}
