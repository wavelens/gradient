/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import { RowComponent } from './row.component';
import { RowListComponent } from './row-list.component';

describe('gr-row', () => {
  beforeEach(() => TestBed.configureTestingModule({ providers: [provideRouter([])] }));

  async function render(inputs: Record<string, unknown> = {}) {
    const fixture = TestBed.createComponent(RowComponent);
    for (const [k, v] of Object.entries(inputs)) fixture.componentRef.setInput(k, v);
    fixture.detectChanges();
    await fixture.whenStable();
    return fixture.nativeElement as HTMLElement;
  }

  it('renders the row scaffold', async () => {
    const root = await render();
    expect(root.querySelector('.row-name')).not.toBeNull();
    expect(root.querySelector('.row-meta')).not.toBeNull();
    expect(root.querySelector('.row-actions')).not.toBeNull();
  });

  it('omits the icon by default', async () => {
    expect((await render()).querySelector('gr-icon')).toBeNull();
  });

  it('renders the icon it is given', async () => {
    expect((await render({ icon: 'key' })).querySelector('gr-icon')).not.toBeNull();
  });

  it('is not muted by default', async () => {
    expect((await render()).querySelector('.row.is-muted')).toBeNull();
  });

  it('marks itself muted on request', async () => {
    expect((await render({ muted: true })).querySelector('.row.is-muted')).not.toBeNull();
  });

  it('is not a link by default, so it offers no hover affordance', async () => {
    const root = await render();
    expect(root.querySelector('a')).toBeNull();
    expect(root.querySelector('.row-chevron')).toBeNull();
    expect(root.querySelector('.row')?.classList).not.toContain('is-link');
  });

  it('turns the whole row into a link when given a destination', async () => {
    const root = await render({ link: ['/caches', 'nixpkgs', 'members-roles'] });
    const anchor = root.querySelector('a');
    expect(anchor?.getAttribute('href')).toBe('/caches/nixpkgs/members-roles');
    expect(root.querySelector('.row-chevron')).not.toBeNull();
  });

  it('keeps a linked row to one anchor, so actions stay reachable', async () => {
    const root = await render({ link: ['/x'] });
    expect(root.querySelectorAll('a')).toHaveLength(1);
  });
});

@Component({
  standalone: true,
  imports: [RowComponent],
  template: `
    <gr-row icon="group" [link]="['/settings']">
      Members
      <span slot="meta">Who can do what</span>
      <button slot="actions">Edit</button>
    </gr-row>
  `,
})
class LinkedHost {}

describe('gr-row projection into a linked row', () => {
  it('projects name, meta and actions inside the anchor', async () => {
    TestBed.configureTestingModule({ imports: [LinkedHost], providers: [provideRouter([])] });
    const fixture = TestBed.createComponent(LinkedHost);
    fixture.detectChanges();
    await fixture.whenStable();
    fixture.detectChanges();
    const root = fixture.nativeElement as HTMLElement;
    expect(root.querySelector('.row-name')?.textContent).toContain('Members');
    expect(root.querySelector('.row-meta')?.textContent).toContain('Who can do what');
    expect(root.querySelector('.row-actions button')?.textContent).toContain('Edit');
  });
});

@Component({
  standalone: true,
  imports: [RowComponent, RowListComponent],
  template: `
    <gr-row>standalone</gr-row>
    <gr-row-list><gr-row>in a list</gr-row></gr-row-list>
  `,
})
class PaddingHost {}

describe('gr-row padding', () => {
  it('pads and fills only inside a row list', async () => {
    TestBed.configureTestingModule({ imports: [PaddingHost], providers: [provideRouter([])] });
    const fixture = TestBed.createComponent(PaddingHost);
    fixture.detectChanges();
    await fixture.whenStable();
    const rows = (fixture.nativeElement as HTMLElement).querySelectorAll('gr-row');
    // jsdom has no CSS engine, so assert the hook the stylesheet keys on.
    expect(rows[0].closest('gr-row-list')).toBeNull();
    expect(rows[1].closest('gr-row-list')).not.toBeNull();
  });
});
