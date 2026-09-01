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
} from '@shared/ui';

@Component({
  selector: 'app-sg-components',
  standalone: true,
  imports: [
    FormsModule, ButtonComponent, CheckboxComponent, SelectComponent, SelectButtonComponent,
    AutoCompleteComponent, MenuComponent, PopoverComponent, TooltipDirective, InputDirective,
    FormFieldComponent, LabelHelpComponent, DialogComponent, PasswordInputComponent,
  ],
  templateUrl: './components.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './demo.scss',
})
export class ComponentsComponent {
  name = signal('');
  password = signal('');
  nameInvalid = signal(false);
  accepted = signal(false);
  region = signal('eu');
  dialogOpen = signal(false);
  suggestions = ['europe', 'north-america', 'asia'];
  regions = [
    { label: 'Europe', value: 'eu' },
    { label: 'North America', value: 'na' },
  ];
  menuItems: MenuItem[] = [
    { label: 'Rebuild', icon: 'refresh' },
    { separator: true },
    { label: 'Delete', icon: 'delete' },
  ];
}
