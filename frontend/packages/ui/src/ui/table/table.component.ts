/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, ChangeDetectionStrategy } from '@angular/core';
import { ScrollRegionDirective } from '../scroll-region/scroll-region.directive';

@Component({
  selector: 'gr-table',
  standalone: true,
  imports: [ScrollRegionDirective],
  template: `
    <div class="gr-table__scroll" grScrollRegion>
      <table><ng-content></ng-content></table>
    </div>
  `,
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './table.component.scss',
})
export class TableComponent {}
