/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, signal } from '@angular/core';
import { FormsModule } from '@angular/forms';
import { TestBed } from '@angular/core/testing';
import { AutoCompleteComponent, AutoCompleteSuggestion } from './autocomplete.component';

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
  suggestions = signal<(string | AutoCompleteSuggestion)[]>([]);
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

  it('stays closed on focus and lists all suggestions on click or ArrowDown', async () => {
    const { fixture, input } = await render();
    fixture.componentInstance.suggestions.set(['my-cache']);
    input().dispatchEvent(new Event('focus'));
    fixture.detectChanges();
    expect(options()).toHaveLength(0);

    input().click();
    fixture.detectChanges();
    expect(fixture.componentInstance.queries()).toEqual(['']);
    expect(options()).toHaveLength(1);

    input().dispatchEvent(new Event('blur'));
    fixture.detectChanges();
    expect(options()).toHaveLength(0);
    input().dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowDown' }));
    fixture.detectChanges();
    expect(options()).toHaveLength(1);
  });

  it('shows labels, writes values and names its listbox for screen readers', async () => {
    const { fixture, type, input } = await render();
    fixture.componentInstance.suggestions.set([{ label: 'QA Team', value: 'qa-team' }, { label: 'Ops', value: 'ops' }]);
    type('q');
    const listbox = document.querySelector('[role=listbox]')!;
    expect(input().getAttribute('aria-controls')).toBe(listbox.id);
    expect(listbox.getAttribute('aria-label')).toBe('e.g. my-cache');
    expect(options().map((o) => o.textContent!.trim())).toEqual(['QA Team', 'Ops']);

    input().dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowDown' }));
    fixture.detectChanges();
    expect(input().getAttribute('aria-activedescendant')).toBe(options()[0].id);
    input().dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', cancelable: true }));
    fixture.detectChanges();
    await fixture.whenStable();
    expect(fixture.componentInstance.name()).toBe('qa-team');
    expect(input().value).toBe('QA Team');
    expect(fixture.componentInstance.entered()).toBe(0);
  });

  it('closes on Escape without letting a surrounding dialog close', async () => {
    const { fixture, type, input } = await render();
    fixture.componentInstance.suggestions.set(['my-cache']);
    type('my');
    const escape = new KeyboardEvent('keydown', { key: 'Escape', bubbles: true });
    const reachedDocument = vi.fn();
    document.addEventListener('keydown', reachedDocument);
    input().dispatchEvent(escape);
    document.removeEventListener('keydown', reachedDocument);
    fixture.detectChanges();
    expect(options()).toHaveLength(0);
    expect(reachedDocument).not.toHaveBeenCalled();
  });

  it('closes its panel when the field goes away', async () => {
    const { fixture, type } = await render();
    fixture.componentInstance.suggestions.set(['my-cache']);
    type('my');
    expect(options()).toHaveLength(1);
    fixture.destroy();
    expect(options()).toHaveLength(0);
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

  it('shows the text below the label in the list but writes only the label into the field', async () => {
    const { fixture, type, input } = await render();
    fixture.componentInstance.suggestions.set([{ label: 'QA Team', value: 'qa-team', textBelowLabel: '3 members' }]);
    type('q');
    expect(options()[0].querySelector('.gr-autocomplete__option-text-below-label')?.textContent).toBe('3 members');
    options()[0].click();
    fixture.detectChanges();
    await fixture.whenStable();
    expect(input().value).toBe('QA Team');
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
    input().dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter' }));
    fixture.detectChanges();
    expect(fixture.componentInstance.entered()).toBe(1);
  });
});
