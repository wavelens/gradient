/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, signal } from '@angular/core';
import { FormsModule } from '@angular/forms';
import { TestBed } from '@angular/core/testing';
import { SelectComponent } from './select.component';

@Component({
  standalone: true,
  imports: [SelectComponent, FormsModule],
  template: `
    <gr-select
      inputId="pick"
      [options]="options()"
      optionLabel="label"
      optionValue="value"
      [optionDisabled]="'inactive'"
      placeholder="Any project"
      [ngModel]="chosen()"
      (ngModelChange)="chosen.set($event)"
      [disabled]="disabled()"
    ></gr-select>
  `,
})
class HostComponent {
  options = signal([
    { label: 'Alpha', value: 'a' },
    { label: 'Beta', value: 'b' },
    { label: 'Gone', value: 'c', inactive: true },
  ]);
  chosen = signal<string | null>(null);
  disabled = signal(false);
}

async function render() {
  TestBed.configureTestingModule({ imports: [HostComponent] });
  const fixture = TestBed.createComponent(HostComponent);
  fixture.detectChanges();
  await fixture.whenStable();
  fixture.detectChanges();
  return fixture;
}

const trigger = (f: { nativeElement: HTMLElement }) =>
  f.nativeElement.querySelector('.gr-select__trigger') as HTMLButtonElement;
const optionEls = () => Array.from(document.querySelectorAll('.gr-select__option')) as HTMLElement[];

describe('SelectComponent', () => {
  it('shows the placeholder until something is chosen', async () => {
    const fixture = await render();
    expect(trigger(fixture).textContent).toContain('Any project');
  });

  it('opens a listbox of the options on click', async () => {
    const fixture = await render();
    expect(optionEls()).toHaveLength(0);
    trigger(fixture).click();
    fixture.detectChanges();
    expect(optionEls().map((o) => o.textContent!.trim())).toEqual(['Alpha', 'Beta', 'Gone']);
  });

  it('writes the option value back through ngModel and closes', async () => {
    const fixture = await render();
    trigger(fixture).click();
    fixture.detectChanges();
    optionEls()[1].click();
    fixture.detectChanges();
    await fixture.whenStable();
    expect(fixture.componentInstance.chosen()).toBe('b');
    expect(optionEls()).toHaveLength(0);
    expect(trigger(fixture).textContent).toContain('Beta');
  });

  it('renders the label of a value pushed in from the model', async () => {
    const fixture = await render();
    fixture.componentInstance.chosen.set('a');
    fixture.detectChanges();
    await fixture.whenStable();
    fixture.detectChanges();
    expect(trigger(fixture).textContent).toContain('Alpha');
  });

  it('ignores clicks on a disabled option', async () => {
    const fixture = await render();
    trigger(fixture).click();
    fixture.detectChanges();
    optionEls()[2].click();
    fixture.detectChanges();
    expect(fixture.componentInstance.chosen()).toBeNull();
  });

  it('does not open while disabled', async () => {
    const fixture = await render();
    fixture.componentInstance.disabled.set(true);
    fixture.detectChanges();
    await fixture.whenStable();
    fixture.detectChanges();
    expect(trigger(fixture).disabled).toBe(true);
    trigger(fixture).click();
    fixture.detectChanges();
    expect(optionEls()).toHaveLength(0);
  });

  it('closes on Escape', async () => {
    const fixture = await render();
    trigger(fixture).click();
    fixture.detectChanges();
    trigger(fixture).dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape' }));
    fixture.detectChanges();
    expect(optionEls()).toHaveLength(0);
  });

  it('selects the active option with the keyboard', async () => {
    const fixture = await render();
    trigger(fixture).dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowDown' }));
    fixture.detectChanges();
    trigger(fixture).dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowDown' }));
    fixture.detectChanges();
    trigger(fixture).dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter' }));
    fixture.detectChanges();
    await fixture.whenStable();
    expect(fixture.componentInstance.chosen()).toBe('b');
  });

  it('marks itself a combobox and tracks its expanded state', async () => {
    const fixture = await render();
    expect(trigger(fixture).getAttribute('role')).toBe('combobox');
    expect(trigger(fixture).getAttribute('aria-expanded')).toBe('false');
    trigger(fixture).click();
    fixture.detectChanges();
    expect(trigger(fixture).getAttribute('aria-expanded')).toBe('true');
  });
});
