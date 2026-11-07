/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { LoadStateComponent, loadErrorText } from './load-state.component';

@Component({
  imports: [LoadStateComponent],
  template: `
    <gr-load-state [loading]="loading()" [error]="error()" [empty]="items().length === 0"
                   loadingMessage="Loading things..." emptyTitle="No things yet" errorTitle="Couldn't load things"
                   (retry)="retried = retried + 1">
      <button slot="empty-action">Create thing</button>
      <ul>@for (item of items(); track item) { <li>{{ item }}</li> }</ul>
    </gr-load-state>
  `,
})
class HostComponent {
  loading = signal(true);
  error = signal<unknown>(null);
  items = signal<string[]>([]);
  retried = 0;
}

function render() {
  TestBed.configureTestingModule({ imports: [HostComponent] });
  const fixture = TestBed.createComponent(HostComponent);
  fixture.detectChanges();
  const text = () => {
    fixture.detectChanges();
    return (fixture.nativeElement as HTMLElement).textContent ?? '';
  };
  return { host: fixture.componentInstance, fixture, text };
}

describe('LoadStateComponent', () => {
  it('shows the spinner while the first load runs', () => {
    const { text } = render();
    expect(text()).toContain('Loading things...');
  });

  it('never shows the empty state after a failed load', () => {
    const { host, text } = render();
    host.loading.set(false);
    host.error.set({ status: 500, message: 'database is down' });
    expect(text()).toContain("Couldn't load things");
    expect(text()).toContain('database is down');
    expect(text()).not.toContain('No things yet');
  });

  it('asks the caller to load again on Try Again', () => {
    const { host, fixture, text } = render();
    host.loading.set(false);
    host.error.set(new Error('boom'));
    text();
    (fixture.nativeElement as HTMLElement).querySelector<HTMLButtonElement>('.gr-load-state__error button')?.click();
    expect(host.retried).toBe(1);
  });

  it('shows the empty state with its action when the load found nothing', () => {
    const { host, text } = render();
    host.loading.set(false);
    expect(text()).toContain('No things yet');
    expect(text()).toContain('Create thing');
  });

  it('keeps the data on screen while it reloads', () => {
    const { host, text } = render();
    host.items.set(['a', 'b']);
    expect(text()).toContain('a');
    expect(text()).not.toContain('Loading things...');
  });

  it('explains a refused load instead of repeating the server code', () => {
    expect(loadErrorText({ status: 403, message: 'Forbidden' })).toContain('No permission');
    expect(loadErrorText({ status: 500 })).toBe('The server gave no reason. Try again.');
    expect(loadErrorText('plain')).toBe('plain');
  });
});
