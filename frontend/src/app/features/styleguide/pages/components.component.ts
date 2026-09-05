/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, signal, ChangeDetectionStrategy } from '@angular/core';
import { FormsModule } from '@angular/forms';
import {
  ButtonComponent, CheckboxComponent, SelectComponent, SelectButtonComponent,
  AutoCompleteComponent, MenuComponent, MenuItem, PopoverComponent, TooltipDirective,
  InputDirective, FormFieldComponent, LabelHelpComponent, DialogComponent, PasswordInputComponent,
  NameFieldComponent, NameCheckState, TabSwitchComponent, TableComponent,
} from '@shared/ui';

@Component({
  selector: 'app-sg-components',
  standalone: true,
  imports: [
    FormsModule, ButtonComponent, CheckboxComponent, SelectComponent, SelectButtonComponent,
    AutoCompleteComponent, MenuComponent, PopoverComponent, TooltipDirective, InputDirective,
    FormFieldComponent, LabelHelpComponent, DialogComponent, PasswordInputComponent, NameFieldComponent,
    TabSwitchComponent, TableComponent,
  ],
  templateUrl: './components.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './demo.scss',
})
export class ComponentsComponent {
  name = signal('');
  slug = signal('gradient');
  nameStates: NameCheckState[] = ['idle', 'checking', 'available', 'taken', 'invalid', 'reserved'];
  password = signal('');
  nameInvalid = signal(false);
  accepted = signal(false);
  region = signal('eu');
  window = signal('hours');
  windows = [
    { label: 'Minutes', value: 'minutes' },
    { label: 'Hours', value: 'hours' },
    { label: 'Days', value: 'days' },
    { label: 'Weeks', value: 'weeks' },
  ];
  dialogOpen = signal(false);
  suggestions = ['europe', 'north-america', 'asia'];
  regions = [
    { label: 'Europe', value: 'eu' },
    { label: 'North America', value: 'na' },
  ];
  workerRows = [
    { worker: 'builder-01', state: 'active', builds: 12 },
    { worker: 'builder-02', state: 'draining', builds: 3 },
    { worker: 'builder-03', state: 'active', builds: 0 },
  ];
  menuItems: MenuItem[] = [
    { label: 'Rebuild', icon: 'refresh' },
    { separator: true },
    { label: 'Delete', icon: 'delete' },
  ];
}
