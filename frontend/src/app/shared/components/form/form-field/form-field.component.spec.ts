/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { FormControl, Validators } from '@angular/forms';
import { FormFieldComponent } from './form-field.component';

describe('FormFieldComponent', () => {
  beforeEach(async () => {
    await TestBed.configureTestingModule({ imports: [FormFieldComponent] }).compileComponents();
  });

  it('renders label and required marker', async () => {
    const fixture = TestBed.createComponent(FormFieldComponent);
    fixture.componentRef.setInput('label', 'Username');
    fixture.componentRef.setInput('required', true);
    fixture.detectChanges();
    await fixture.whenStable();
    const root = fixture.nativeElement as HTMLElement;
    expect(root.querySelector('label')?.textContent).toContain('Username');
    expect(root.querySelector('.required')).not.toBeNull();
  });

  it('adds has-error class when control is touched + invalid', async () => {
    const fixture = TestBed.createComponent(FormFieldComponent);
    const ctrl = new FormControl('', Validators.required);
    ctrl.markAsTouched();
    fixture.componentRef.setInput('control', ctrl);
    fixture.detectChanges();
    await fixture.whenStable();
    expect((fixture.nativeElement as HTMLElement).querySelector('.form-field.has-error')).not.toBeNull();
  });
});
