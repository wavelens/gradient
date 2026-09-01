/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, signal } from '@angular/core';
import { FormsModule } from '@angular/forms';
import { TestBed } from '@angular/core/testing';
import { SelectButtonComponent } from './select-button.component';

@Component({
  standalone: true,
  imports: [SelectButtonComponent, FormsModule],
  template: `
    <gr-select-button
      [options]="options"
      optionLabel="label"
      optionValue="value"
      [allowEmpty]="allowEmpty()"
      ariaLabel="Scope"
      [ngModel]="scope()"
      (ngModelChange)="scope.set($event)"
    ></gr-select-button>
  `,
})
class HostComponent {
  options = [
    { label: 'Mine', value: 'mine' },
    { label: 'All', value: 'all' },
  ];
  scope = signal<string | null>('mine');
  allowEmpty = signal(false);
}

async function render() {
  TestBed.configureTestingModule({ imports: [HostComponent] });
  const fixture = TestBed.createComponent(HostComponent);
  fixture.detectChanges();
  await fixture.whenStable();
  fixture.detectChanges();
  const items = () => Array.from(fixture.nativeElement.querySelectorAll('.gr-select-button__item')) as HTMLButtonElement[];
  return { fixture, items };
}

describe('SelectButtonComponent', () => {
  it('renders one radio per option and marks the selected one', async () => {
    const { items } = await render();
    expect(items().map((i) => i.textContent!.trim())).toEqual(['Mine', 'All']);
    expect(items()[0].getAttribute('aria-checked')).toBe('true');
    expect(items()[1].getAttribute('aria-checked')).toBe('false');
  });

  it('names the group so a screen reader can announce it', async () => {
    const { fixture } = await render();
    const group = fixture.nativeElement.querySelector('[role=radiogroup]') as HTMLElement;
    expect(group.getAttribute('aria-label')).toBe('Scope');
  });

  it('writes the picked option value back', async () => {
    const { fixture, items } = await render();
    items()[1].click();
    fixture.detectChanges();
    await fixture.whenStable();
    expect(fixture.componentInstance.scope()).toBe('all');
  });

  it('keeps the selection when re-clicked and empty is not allowed', async () => {
    const { fixture, items } = await render();
    items()[0].click();
    fixture.detectChanges();
    await fixture.whenStable();
    expect(fixture.componentInstance.scope()).toBe('mine');
  });

  it('clears the selection when re-clicked and empty is allowed', async () => {
    const { fixture, items } = await render();
    fixture.componentInstance.allowEmpty.set(true);
    fixture.detectChanges();
    items()[0].click();
    fixture.detectChanges();
    await fixture.whenStable();
    expect(fixture.componentInstance.scope()).toBeNull();
  });
});
