/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, input, ChangeDetectionStrategy } from '@angular/core';
import { BreadcrumbComponent, type Crumb } from '../breadcrumb/breadcrumb.component';

export type PageWidth = 'form' | 'settings' | 'detail' | 'full';

@Component({
  selector: 'gr-page-layout',
  standalone: true,
  imports: [BreadcrumbComponent],
  templateUrl: './page-layout.component.html',
  host: { '[attr.title]': 'null' },
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './page-layout.component.scss',
})
export class PageLayoutComponent {
  breadcrumb = input<Crumb[]>([]);
  title = input.required<string>();
  subtitle = input<string>();
  /// Content widths of page types are `form` for creation flows, `settings` for all settings pages, `detail` for overviews and lists, and `full` for graphs and logs.
  width = input<PageWidth>('detail');
}
