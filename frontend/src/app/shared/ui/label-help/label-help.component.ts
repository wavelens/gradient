/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, computed, input, ChangeDetectionStrategy } from '@angular/core';
import { DocLink, docsUrl } from '@core/docs';
import { IconComponent } from '@gradient/ui/ui';
import { CommonModule } from '@angular/common';

@Component({
  selector: 'gr-label-help',
  standalone: true,
  imports: [IconComponent, CommonModule],
  template: `
    <a
      class="label-help"
      [href]="href()"
      target="_blank"
      rel="noopener noreferrer"
      [title]="title()"
      [attr.aria-label]="title()"
    >
      <gr-icon name="help" size="sm" />
    </a>
  `,
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './label-help.component.scss',
})
export class LabelHelpComponent {
  doc = input.required<DocLink>();
  title = input<string>('Learn more');

  protected readonly href = computed(() => docsUrl(this.doc()));
}
