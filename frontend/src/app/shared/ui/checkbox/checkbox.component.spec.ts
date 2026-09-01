/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, signal } from '@angular/core';
import { FormsModule } from '@angular/forms';
import { TestBed } from '@angular/core/testing';
import { CheckboxComponent } from './checkbox.component';

@Component({
  standalone: true,
  imports: [CheckboxComponent, FormsModule],
  template: `
    <gr-checkbox
      inputId="accept"
      [binary]="true"
      [ngModel]="accepted()"
      (ngModelChange)="accepted.set($event)"
      [disabled]="disabled()"
    ></gr-checkbox>
  `,
})
class HostComponent {
  accepted = signal(false);
  disabled = signal(false);
}

async function render() {
  TestBed.configureTestingModule({ imports: [HostComponent] });
  const fixture = TestBed.createComponent(HostComponent);
  fixture.detectChanges();
  await fixture.whenStable();
  fixture.detectChanges();
  const input = () => fixture.nativeElement.querySelector('input') as HTMLInputElement;
  return { fixture, input };
}

describe('CheckboxComponent', () => {
  it('renders a native checkbox carrying the given id', async () => {
    const { input } = await render();
    expect(input().type).toBe('checkbox');
    expect(input().id).toBe('accept');
  });

  it('reflects the model value', async () => {
    const { fixture, input } = await render();
    expect(input().checked).toBe(false);
    fixture.componentInstance.accepted.set(true);
    fixture.detectChanges();
    await fixture.whenStable();
    fixture.detectChanges();
    expect(input().checked).toBe(true);
  });

  it('writes the new value back on toggle', async () => {
    const { fixture, input } = await render();
    input().click();
    fixture.detectChanges();
    await fixture.whenStable();
    expect(fixture.componentInstance.accepted()).toBe(true);
  });

  it('honours a disabled state pushed in through the form API', async () => {
    const { fixture, input } = await render();
    fixture.componentInstance.disabled.set(true);
    fixture.detectChanges();
    await fixture.whenStable();
    fixture.detectChanges();
    expect(input().disabled).toBe(true);
  });
});
