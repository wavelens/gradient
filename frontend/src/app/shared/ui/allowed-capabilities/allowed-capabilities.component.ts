/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, booleanAttribute, input, model } from '@angular/core';
import { AllowedCapabilities, GradientCapabilities } from '@core/models';
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
          @let unsupported = value()[cap.key] && workerReportedCapabilities()?.[cap.reportedCapabilityKey] === false;
          <span
            class="allowed-pill"
            [class.allowed-pill--on]="value()[cap.key] && !unsupported"
            [class.allowed-pill--unsupported-by-worker]="unsupported"
            [attr.data-allowed]="value()[cap.key]"
            [attr.title]="unsupported ? 'Allowed, but the worker doesn\\'t report support' : null"
          >{{ cap.label }}</span>
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
  workerReportedCapabilities = input<GradientCapabilities>();

  readonly capabilities: { key: keyof AllowedCapabilities; reportedCapabilityKey: keyof GradientCapabilities; label: string }[] = [
    { key: 'enable_network', reportedCapabilityKey: 'network', label: 'network' },
    { key: 'enable_secrets', reportedCapabilityKey: 'secrets', label: 'secrets' },
    { key: 'enable_exec', reportedCapabilityKey: 'exec', label: 'exec' },
    { key: 'enable_eval', reportedCapabilityKey: 'eval', label: 'eval' },
    { key: 'enable_build', reportedCapabilityKey: 'build', label: 'build' },
  ];

  toggle(key: keyof AllowedCapabilities): void {
    this.value.update((value) => ({ ...value, [key]: !value[key] }));
  }
}
