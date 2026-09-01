/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, signal } from '@angular/core';
import { FormsModule } from '@angular/forms';
import { TestBed } from '@angular/core/testing';
import { AutoCompleteComponent } from './autocomplete.component';

@Component({
  standalone: true,
  imports: [AutoCompleteComponent, FormsModule],
  template: `
    <gr-autocomplete
      inputId="cache-name"
      placeholder="e.g. my-cache"
      [suggestions]="suggestions()"
      (completeMethod)="queries.set(queries().concat($event.query))"
      (enter)="entered.set(entered() + 1)"
      [ngModel]="name()"
      (ngModelChange)="name.set($event)"
    ></gr-autocomplete>
  `,
})
class HostComponent {
  suggestions = signal<string[]>([]);
  queries = signal<string[]>([]);
  entered = signal(0);
  name = signal('');
}

async function render() {
  TestBed.configureTestingModule({ imports: [HostComponent] });
  const fixture = TestBed.createComponent(HostComponent);
  fixture.detectChanges();
  await fixture.whenStable();
  fixture.detectChanges();
  const input = () => fixture.nativeElement.querySelector('input') as HTMLInputElement;
  const type = (text: string) => {
    input().value = text;
    input().dispatchEvent(new Event('input'));
    fixture.detectChanges();
  };
  return { fixture, input, type };
}

const options = () => Array.from(document.querySelectorAll('.gr-autocomplete__option')) as HTMLElement[];

describe('AutoCompleteComponent', () => {
  it('asks the caller to complete on every keystroke', async () => {
    const { fixture, type } = await render();
    type('my');
    expect(fixture.componentInstance.queries()).toEqual(['my']);
  });

  it('writes the typed text back as the model value', async () => {
    const { fixture, type } = await render();
    type('my-cache');
    await fixture.whenStable();
    expect(fixture.componentInstance.name()).toBe('my-cache');
  });

  it('shows suggestions only once there are any', async () => {
    const { fixture, type } = await render();
    type('my');
    expect(options()).toHaveLength(0);
    fixture.componentInstance.suggestions.set(['my-cache', 'my-other']);
    fixture.detectChanges();
    expect(options().map((o) => o.textContent!.trim())).toEqual(['my-cache', 'my-other']);
  });

  it('picks a suggestion into the model and closes the panel', async () => {
    const { fixture, type, input } = await render();
    fixture.componentInstance.suggestions.set(['my-cache']);
    type('my');
    options()[0].click();
    fixture.detectChanges();
    await fixture.whenStable();
    expect(fixture.componentInstance.name()).toBe('my-cache');
    expect(input().value).toBe('my-cache');
    expect(options()).toHaveLength(0);
  });

  it('emits enter for callers that submit from the field', async () => {
    const { fixture, input } = await render();
    input().dispatchEvent(new KeyboardEvent('keyup', { key: 'Enter' }));
    fixture.detectChanges();
    expect(fixture.componentInstance.entered()).toBe(1);
  });
});
