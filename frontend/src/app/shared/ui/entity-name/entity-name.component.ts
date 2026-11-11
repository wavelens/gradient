/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, computed, input } from '@angular/core';
import { IconComponent } from '@gradient/ui/ui';
import { entityKindToIconName } from '../entity-icon';
import { formatEntityLabel, getEntityDisplayNameOrName, type DisplayNameAndName } from './entity-name';

@Component({
  selector: 'gr-entity-name',
  standalone: true,
  imports: [IconComponent],
  template: `
    @if (kind(); as entityKind) {
      <gr-icon class="entity-name__icon" [name]="entityKindToIconName[entityKind]" />
    }
    <span class="entity-name__display">{{ shownDisplayName() }}</span>
    @if (shownDisplayName() !== name()) {
      <span class="entity-name__slug">{{ name() }}</span>
    }
    @if (ownerTeam(); as owner) {
      <span class="entity-name__owner">· {{ getEntityDisplayNameOrName(owner) }}</span>
    }
  `,
  host: { '[attr.title]': 'label()' },
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './entity-name.component.scss',
})
export class EntityNameComponent {
  kind = input<keyof typeof entityKindToIconName>();
  displayName = input<string | null>();
  name = input.required<string>();
  ownerTeam = input<DisplayNameAndName | null>();

  protected readonly entityKindToIconName = entityKindToIconName;
  protected shownDisplayName = computed(() => this.displayName()?.trim() || this.name());
  protected readonly getEntityDisplayNameOrName = getEntityDisplayNameOrName;
  protected label = computed(() => formatEntityLabel({ display_name: this.displayName(), name: this.name() }, this.ownerTeam()));
}
