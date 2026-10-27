/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, computed, input, model, output } from '@angular/core';
import { FormsModule } from '@angular/forms';
import { FormFieldComponent, InputDirective, NameFieldComponent } from '@gradient/ui/ui';
import { NameAvailability } from './name-availability';

@Component({
  selector: 'app-name-fields',
  standalone: true,
  imports: [FormsModule, FormFieldComponent, InputDirective, NameFieldComponent],
  changeDetection: ChangeDetectionStrategy.Eager,
  template: `
    <gr-form-field label="Display Name" [for]="noun() + '-display-name'" required>
      <input
        grInput
        [id]="noun() + '-display-name'"
        [placeholder]="'My ' + title()"
        [ngModel]="displayName()"
        [ngModelOptions]="{ standalone: true }"
        (ngModelChange)="onDisplayNameChange($event)"
        (keyup.enter)="submitted.emit()"
      />
    </gr-form-field>
    <gr-name-field
      required
      [inputId]="noun() + '-name'"
      [placeholder]="noun() + '-name'"
      [state]="names().state()"
      [ngModel]="name()"
      [ngModelOptions]="{ standalone: true }"
      (ngModelChange)="onNameChange($event)"
      (keyup.enter)="submitted.emit()"
    />
  `,
})
export class NameFieldsComponent {
  noun = input.required<string>();
  names = input.required<NameAvailability>();
  displayName = model('');
  name = model('');
  submitted = output();

  protected title = computed(() => this.noun().charAt(0).toUpperCase() + this.noun().slice(1));

  protected onDisplayNameChange(value: string): void {
    this.displayName.set(value);
    const name = this.names().suggest(value);
    if (name !== null) this.name.set(name);
  }

  protected onNameChange(value: string): void {
    this.names().markEdited();
    this.name.set(value);
    this.names().check(value);
  }
}
