/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import { NavCardComponent } from './nav-card.component';

describe('gr-nav-card', () => {
  beforeEach(() => TestBed.configureTestingModule({ providers: [provideRouter([])] }));

  async function render(inputs: Record<string, unknown>) {
    const fixture = TestBed.createComponent(NavCardComponent);
    for (const [k, v] of Object.entries(inputs)) fixture.componentRef.setInput(k, v);
    fixture.detectChanges();
    await fixture.whenStable();
    return fixture.nativeElement as HTMLElement;
  }

  it('renders the title and description', async () => {
    const root = await render({ icon: 'storage', title: 'nixpkgs', description: 'Upstream mirror' });
    expect(root.textContent).toContain('nixpkgs');
    expect(root.textContent).toContain('Upstream mirror');
  });

  it('omits the description paragraph when none is given', async () => {
    const root = await render({ icon: 'storage', title: 'nixpkgs' });
    expect(root.querySelector('.nav-card__description')).toBeNull();
  });

  it('navigates through the whole card, not a nested link', async () => {
    const root = await render({ icon: 'storage', title: 'nixpkgs', link: ['/caches', 'nixpkgs'] });
    const anchors = root.querySelectorAll('a');
    expect(anchors).toHaveLength(1);
    expect(anchors[0].getAttribute('href')).toBe('/caches/nixpkgs');
  });

  it('marks a muted card so it reads as secondary', async () => {
    const root = await render({ icon: 'storage', title: 'old', muted: true });
    expect(root.querySelector('.nav-card')?.classList).toContain('is-muted');
  });
});
