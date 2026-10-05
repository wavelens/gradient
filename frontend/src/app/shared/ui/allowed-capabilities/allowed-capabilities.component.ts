/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, booleanAttribute, input, model } from '@angular/core';
import { AllowedCapabilities } from '@core/models';
import { IconComponent } from '@gradient/ui/ui';

@Component({
  selector: 'gr-allowed-capabilities',
  standalone: true,
  imports: [IconComponent],
  template: `
    @if (editable()) {
      <div class="allowed-toggles">
        @for (cap of capabilities; track cap.key) {
          <button
            type="button"
            class="allowed-toggle"
            [class.allowed-toggle--on]="value()[cap.key]"
            [attr.aria-pressed]="value()[cap.key]"
            (click)="toggle(cap.key)"
          >
            <gr-icon [name]="value()[cap.key] ? 'check' : 'close'" size="sm" />
            {{ cap.label }}
          </button>
        }
      </div>
    } @else {
      <div class="allowed-summary">
        Allowed:
        @for (cap of capabilities; track cap.key) {
          <span class="allowed-pill" [class.allowed-pill--on]="value()[cap.key]" [attr.data-allowed]="value()[cap.key]">{{ cap.label }}</span>
        }
      </div>
    }
  `,
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './allowed-capabilities.component.scss',
})
export class AllowedCapabilitiesComponent {
  value = model.required<AllowedCapabilities>();
  editable = input(false, { transform: booleanAttribute });

  readonly capabilities: { key: keyof AllowedCapabilities; label: string }[] = [
    { key: 'enable_fetch', label: 'fetch' },
    { key: 'enable_eval', label: 'eval' },
    { key: 'enable_build', label: 'build' },
  ];

  toggle(key: keyof AllowedCapabilities): void {
    this.value.update((value) => ({ ...value, [key]: !value[key] }));
  }
}
