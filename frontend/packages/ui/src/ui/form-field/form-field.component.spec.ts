/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { FormControl, Validators } from '@angular/forms';
import { FormFieldComponent } from './form-field.component';

@Component({
  standalone: true,
  imports: [FormFieldComponent],
  template: `
    <gr-form-field label="Name" [invalid]="bad()" error="Name is taken" hint="Lowercase only">
      <span slot="label" class="help">?</span>
      <input />
    </gr-form-field>
  `,
})
class TemplateDrivenHost {
  bad = signal(false);
}

describe('gr-form-field', () => {
  async function render<T>(type: new () => T) {
    const fixture = TestBed.createComponent(type);
    fixture.detectChanges();
    await fixture.whenStable();
    return fixture;
  }

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

  it('projects extra label content inside the label', async () => {
    const label = ((await render(TemplateDrivenHost)).nativeElement as HTMLElement).querySelector('label');
    expect(label?.textContent).toContain('Name');
    expect(label?.querySelector('.help')).not.toBeNull();
  });

  it('shows the hint while valid', async () => {
    const root = (await render(TemplateDrivenHost)).nativeElement as HTMLElement;
    expect(root.querySelector('.hint')?.textContent).toContain('Lowercase only');
    expect(root.querySelector('.field-error')).toBeNull();
  });

  it('replaces the hint with the error when invalid is set', async () => {
    const fixture = await render(TemplateDrivenHost);
    fixture.componentInstance.bad.set(true);
    fixture.detectChanges();
    await fixture.whenStable();
    const root = fixture.nativeElement as HTMLElement;
    expect(root.querySelector('.field-error')?.textContent).toContain('Name is taken');
    expect(root.querySelector('.hint')).toBeNull();
  });

  it('still derives errors from a reactive control', async () => {
    const fixture = TestBed.createComponent(FormFieldComponent);
    const ctrl = new FormControl('', Validators.required);
    ctrl.markAsTouched();
    fixture.componentRef.setInput('control', ctrl);
    fixture.componentRef.setInput('errorMessages', { required: 'Required field' });
    fixture.detectChanges();
    await fixture.whenStable();
    const root = fixture.nativeElement as HTMLElement;
    expect(root.querySelector('.form-field.has-error')).not.toBeNull();
    expect(root.querySelector('.field-error')?.textContent).toContain('Required field');
  });

  it('marks the projected control invalid and points it at the message', async () => {
    const fixture = await render(TemplateDrivenHost);
    const input = (fixture.nativeElement as HTMLElement).querySelector('input')!;
    expect(input.getAttribute('aria-invalid')).toBe('false');
    expect(input.getAttribute('aria-describedby')).toBeTruthy();

    fixture.componentInstance.bad.set(true);
    fixture.detectChanges();
    await fixture.whenStable();
    expect(input.getAttribute('aria-invalid')).toBe('true');
  });

  it('announces the error and links it by id', async () => {
    const fixture = await render(TemplateDrivenHost);
    fixture.componentInstance.bad.set(true);
    fixture.detectChanges();
    await fixture.whenStable();
    const root = fixture.nativeElement as HTMLElement;
    const error = root.querySelector('.field-error')!;
    expect(error.getAttribute('role')).toBe('alert');
    expect(root.querySelector('input')!.getAttribute('aria-describedby')).toBe(error.id);
  });

  it('prefers an explicit invalid input over the control state', async () => {
    const fixture = TestBed.createComponent(FormFieldComponent);
    const ctrl = new FormControl('', Validators.required);
    ctrl.markAsTouched();
    fixture.componentRef.setInput('control', ctrl);
    fixture.componentRef.setInput('invalid', false);
    fixture.detectChanges();
    await fixture.whenStable();
    expect((fixture.nativeElement as HTMLElement).querySelector('.has-error')).toBeNull();
  });
});
