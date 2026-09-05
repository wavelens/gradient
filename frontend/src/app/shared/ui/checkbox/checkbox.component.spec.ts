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

describe('gr-checkbox label', () => {
  it('renders no label by default', async () => {
    const fixture = TestBed.createComponent(CheckboxComponent);
    fixture.detectChanges();
    await fixture.whenStable();
    expect((fixture.nativeElement as HTMLElement).querySelector('label')).toBeNull();
  });

  it('renders the label beside the box and links it to the input', async () => {
    const fixture = TestBed.createComponent(CheckboxComponent);
    fixture.componentRef.setInput('inputId', 'accept');
    fixture.componentRef.setInput('label', 'Accept terms');
    fixture.detectChanges();
    await fixture.whenStable();
    const root = fixture.nativeElement as HTMLElement;
    const box = root.querySelector('input')!;
    const label = root.querySelector('label')!;
    expect(label.textContent).toContain('Accept terms');
    expect(label.getAttribute('for')).toBe('accept');
    // beside, not stacked: the label starts to the right of the box
    expect(label.compareDocumentPosition(box) & Node.DOCUMENT_POSITION_PRECEDING).toBeTruthy();
  });

  it('links the label to the box even when the caller gives no id', async () => {
    const fixture = TestBed.createComponent(CheckboxComponent);
    fixture.componentRef.setInput('label', 'Include build logs');
    fixture.detectChanges();
    await fixture.whenStable();
    const root = fixture.nativeElement as HTMLElement;
    const box = root.querySelector('input')!;
    expect(box.id).not.toBe('');
    expect(root.querySelector('label')!.getAttribute('for')).toBe(box.id);
  });

  it('gives two unnamed boxes different ids, so one label cannot toggle both', async () => {
    const first = TestBed.createComponent(CheckboxComponent);
    first.componentRef.setInput('label', 'One');
    first.detectChanges();
    const second = TestBed.createComponent(CheckboxComponent);
    second.componentRef.setInput('label', 'Two');
    second.detectChanges();
    await second.whenStable();
    const id = (f: typeof first) => (f.nativeElement as HTMLElement).querySelector('input')!.id;
    expect(id(first)).not.toBe(id(second));
  });

  it('clicking the label toggles the box', async () => {
    const fixture = TestBed.createComponent(CheckboxComponent);
    fixture.componentRef.setInput('label', 'Include build logs');
    fixture.detectChanges();
    await fixture.whenStable();
    const root = fixture.nativeElement as HTMLElement;
    document.body.appendChild(root);
    root.querySelector('label')!.click();
    fixture.detectChanges();
    expect(root.querySelector('input')!.checked).toBe(true);
    root.remove();
  });
});

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
