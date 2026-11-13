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

@Component({
  standalone: true,
  imports: [FormFieldComponent],
  template: `
    <gr-form-field label="Keep Evaluations">
      <span slot="hint">Instance limit: 30</span>
      <input />
    </gr-form-field>
  `,
})
class VisibleHintHost {}

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

  it('places extra label content beside the label, out of the field name', async () => {
    const root = (await render(TemplateDrivenHost)).nativeElement as HTMLElement;
    const label = root.querySelector('label');
    expect(label?.textContent?.trim()).toBe('Name');
    expect(label?.querySelector('.help')).toBeNull();
    expect(root.querySelector('.form-field__label .help')).not.toBeNull();
  });

  it('turns the hint into an explanation button after the label instead of a line under the field', async () => {
    const root = (await render(TemplateDrivenHost)).nativeElement as HTMLElement;
    const button = root.querySelector('.form-field__label gr-label-explanation-button button')!;
    expect(button.getAttribute('aria-label')).toBe('About Name');
    expect(document.getElementById(button.getAttribute('aria-describedby')!)?.textContent).toBe('Lowercase only');
    expect(root.querySelector('small.hint')?.textContent?.trim()).toBe('');
    expect(root.querySelector('.field-error')).toBeNull();
  });

  it('places the explanation button beside the control when the field has no label', async () => {
    const fixture = TestBed.createComponent(FormFieldComponent);
    fixture.componentRef.setInput('hint', 'For caches that break HTTP/2 transfers.');
    fixture.detectChanges();
    await fixture.whenStable();
    const root = fixture.nativeElement as HTMLElement;
    expect(root.querySelector('.form-field__label')).toBeNull();
    expect(root.querySelector('.form-field__control-wrapper gr-label-explanation-button')).not.toBeNull();
  });

  it('keeps projected hint content visible under the field', async () => {
    const root = (await render(VisibleHintHost)).nativeElement as HTMLElement;
    expect(root.querySelector('small.hint')?.textContent).toContain('Instance limit: 30');
    expect(root.querySelector('gr-label-explanation-button')).toBeNull();
  });

  it('describes the control by the hint and by the visible message', async () => {
    const fixture = await render(TemplateDrivenHost);
    fixture.componentInstance.bad.set(true);
    fixture.detectChanges();
    await fixture.whenStable();
    const root = fixture.nativeElement as HTMLElement;
    const describedBy = root.querySelector('input')!.getAttribute('aria-describedby')!.split(' ');
    const hintId = root.querySelector('.form-field > span[hidden]')!.id;
    expect(describedBy).toEqual([hintId, root.querySelector('.field-error')!.id]);
  });

  it('shows the error under the field and keeps the explanation button', async () => {
    const fixture = await render(TemplateDrivenHost);
    fixture.componentInstance.bad.set(true);
    fixture.detectChanges();
    await fixture.whenStable();
    const root = fixture.nativeElement as HTMLElement;
    expect(root.querySelector('.field-error')?.textContent).toContain('Name is taken');
    expect(root.querySelector('small.hint')).toBeNull();
    expect(root.querySelector('gr-label-explanation-button')).not.toBeNull();
  });

  it('shows a warning under the field and the error over it', async () => {
    const fixture = TestBed.createComponent(FormFieldComponent);
    fixture.componentRef.setInput('hint', 'Lowercase only');
    fixture.componentRef.setInput('warning', 'Not pinned');
    fixture.detectChanges();
    await fixture.whenStable();
    const root = fixture.nativeElement as HTMLElement;
    expect(root.querySelector('.field-warning')?.textContent).toContain('Not pinned');
    expect(root.querySelector('small.hint')).toBeNull();

    fixture.componentRef.setInput('invalid', true);
    fixture.componentRef.setInput('error', 'Required');
    fixture.detectChanges();
    await fixture.whenStable();
    expect(root.querySelector('.field-error')?.textContent).toContain('Required');
    expect(root.querySelector('.field-warning')).toBeNull();
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

  it('shows the error once the user leaves an empty required field', async () => {
    const fixture = TestBed.createComponent(FormFieldComponent);
    const ctrl = new FormControl('', Validators.required);
    fixture.componentRef.setInput('control', ctrl);
    fixture.componentRef.setInput('errorMessages', { required: 'Username is required' });
    fixture.detectChanges();
    await fixture.whenStable();
    const root = fixture.nativeElement as HTMLElement;
    expect(root.querySelector('.field-error')).toBeNull();

    ctrl.markAsTouched();
    fixture.detectChanges();
    expect(root.querySelector('.field-error')?.textContent).toContain('Username is required');

    ctrl.setValue('alice');
    fixture.detectChanges();
    expect(root.querySelector('.field-error')).toBeNull();
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
    expect(root.querySelector('input')!.getAttribute('aria-describedby')).toContain(error.id);
  });

  it('gives each field without a for id its own message id', async () => {
    const first = (await render(TemplateDrivenHost)).nativeElement as HTMLElement;
    const second = (await render(TemplateDrivenHost)).nativeElement as HTMLElement;
    const firstId = first.querySelector('input')!.getAttribute('aria-describedby');
    expect(firstId).toBeTruthy();
    expect(second.querySelector('input')!.getAttribute('aria-describedby')).not.toBe(firstId);
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
