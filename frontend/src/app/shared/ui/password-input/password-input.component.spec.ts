/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, signal } from '@angular/core';
import { FormControl, FormsModule, ReactiveFormsModule } from '@angular/forms';
import { TestBed } from '@angular/core/testing';
import { PasswordInputComponent } from './password-input.component';

@Component({
  standalone: true,
  imports: [PasswordInputComponent, FormsModule],
  template: `
    <gr-password-input
      inputId="pw"
      [ngModel]="secret()"
      (ngModelChange)="secret.set($event)"
      [disabled]="disabled()"
      name="secret"
    ></gr-password-input>
  `,
})
class TemplateDrivenHost {
  secret = signal('');
  disabled = signal(false);
}

@Component({
  standalone: true,
  imports: [PasswordInputComponent, ReactiveFormsModule],
  template: `<gr-password-input inputId="pw" [formControl]="control" />`,
})
class ReactiveHost {
  control = new FormControl('');
}

async function render<T>(host: new () => T) {
  TestBed.configureTestingModule({ imports: [host as never] });
  const fixture = TestBed.createComponent(host);
  fixture.detectChanges();
  await fixture.whenStable();
  fixture.detectChanges();
  const input = () => fixture.nativeElement.querySelector('input') as HTMLInputElement;
  const toggle = () =>
    fixture.nativeElement.querySelector('.password-input__toggle') as HTMLButtonElement;
  return { fixture, input, toggle };
}

describe('PasswordInputComponent', () => {
  it('toggles visibility on button click', async () => {
    const { fixture, input, toggle } = await render(TemplateDrivenHost);
    expect(input().type).toBe('password');
    toggle().click();
    fixture.detectChanges();
    await fixture.whenStable();
    expect(input().type).toBe('text');
  });

  it('carries the given id so a label can point at it', async () => {
    const { input } = await render(TemplateDrivenHost);
    expect(input().id).toBe('pw');
  });

  it('writes typed text back through ngModel', async () => {
    const { fixture, input } = await render(TemplateDrivenHost);
    input().value = 'hunter2';
    input().dispatchEvent(new Event('input'));
    fixture.detectChanges();
    await fixture.whenStable();
    expect(fixture.componentInstance.secret()).toBe('hunter2');
  });

  it('reflects a value pushed in from the model', async () => {
    const { fixture, input } = await render(TemplateDrivenHost);
    fixture.componentInstance.secret.set('from-model');
    fixture.detectChanges();
    await fixture.whenStable();
    fixture.detectChanges();
    expect(input().value).toBe('from-model');
  });

  it('honours a disabled state pushed in through the form API', async () => {
    const { fixture, input } = await render(TemplateDrivenHost);
    fixture.componentInstance.disabled.set(true);
    fixture.detectChanges();
    await fixture.whenStable();
    fixture.detectChanges();
    expect(input().disabled).toBe(true);
  });

  it('still binds to a reactive FormControl', async () => {
    const { fixture, input } = await render(ReactiveHost);
    fixture.componentInstance.control.setValue('reactive');
    fixture.detectChanges();
    await fixture.whenStable();
    fixture.detectChanges();
    expect(input().value).toBe('reactive');
  });
});
