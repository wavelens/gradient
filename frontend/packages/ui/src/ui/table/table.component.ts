/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, ChangeDetectionStrategy } from '@angular/core';

@Component({
  selector: 'gr-table',
  standalone: true,
  template: `
    <div class="gr-table__scroll">
      <table><ng-content></ng-content></table>
    </div>
  `,
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './table.component.scss',
})
export class TableComponent {}
