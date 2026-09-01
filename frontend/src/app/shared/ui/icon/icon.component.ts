/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, input, ChangeDetectionStrategy } from '@angular/core';

export type IconSize = 'xs' | 'sm' | 'md' | 'lg' | 'xl' | 'xxl';

/// Decorative by default; pass a label to expose the icon to assistive tech.
@Component({
  selector: 'gr-icon',
  standalone: true,
  template: `
    <span
      class="material-symbols-outlined gr-icon"
      [class]="'gr-icon--' + size()"
      [attr.role]="label() ? 'img' : null"
      [attr.aria-label]="label() ?? null"
      [attr.aria-hidden]="label() ? null : 'true'"
      >{{ name() }}</span
    >
  `,
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './icon.component.scss',
})
export class IconComponent {
  name = input.required<string>();
  size = input<IconSize>('md');
  label = input<string>();
}
