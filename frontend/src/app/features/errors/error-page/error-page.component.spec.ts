/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { ActivatedRoute, convertToParamMap, provideRouter } from '@angular/router';
import { of } from 'rxjs';
import { AuthService } from '@core/services/auth.service';
import { PendingInvitesService } from '@core/services/pending-invites.service';
import { ErrorPageComponent } from './error-page.component';

function render(code: number, query: Record<string, string>, user: { username: string } | null, pendingInvites = 0) {
  const logout = vi.fn(() => of(null));
  TestBed.configureTestingModule({
    imports: [ErrorPageComponent],
    providers: [
      provideRouter([]),
      { provide: AuthService, useValue: { user: signal(user), logout } },
      { provide: PendingInvitesService, useValue: { count: () => pendingInvites } },
      { provide: ActivatedRoute, useValue: { snapshot: { data: { code }, queryParamMap: convertToParamMap(query) } } },
    ],
  });
  const fixture = TestBed.createComponent(ErrorPageComponent);
  fixture.detectChanges();
  const element = fixture.nativeElement as HTMLElement;
  const links = [...element.querySelectorAll('a, button')].map((el) => ({
    text: el.textContent?.trim(),
    href: el.getAttribute('href'),
  }));
  return { text: element.textContent ?? '', links, logout, fixture };
}

const DENIED = { kind: 'cache', name: 'researchcache', back: '/caches', from: '/caches/researchcache/nars' };

describe('ErrorPageComponent', () => {
  it('names what the link pointed to and offers sign-in to an anonymous visitor', () => {
    const { text, links } = render(404, DENIED, null);
    expect(text).toContain('Not Found or Access Denied');
    expect(text).toContain('Cache "researchcache" not found');
    expect(links).toContainEqual({ text: 'Sign In', href: '/account/login?next=%2Fcaches%2Fresearchcache%2Fnars' });
    expect(links).toContainEqual({ text: 'Back to Caches', href: '/caches' });
  });

  it('tells a signed-in user which account they use and lets them switch', () => {
    const { text, links, logout, fixture } = render(404, DENIED, { username: 'carol' });
    expect(text).toContain('Signed in as carol.');
    expect(links.map((l) => l.text)).not.toContain('Sign In');
    (fixture.nativeElement as HTMLElement).querySelector<HTMLButtonElement>('button')?.click();
    expect(logout).toHaveBeenCalledWith('/caches/researchcache/nars');
  });

  it('points a signed-in invitee to their pending invitations first', () => {
    const { links } = render(404, DENIED, { username: 'carol' }, 2);
    expect(links.map((l) => l.text)).toEqual(['Review Invitations', 'Back to Caches', 'Switch Account']);
    expect(links[0].href).toBe('/settings/invites');
  });

  it('explains admin-only pages instead of redirecting', () => {
    const { text, links } = render(403, { from: '/admin/webhooks' }, { username: 'bob' });
    expect(text).toContain('Admin Only');
    expect(text).toContain('Signed in as bob.');
    expect(links.map((l) => l.text)).toEqual(['Go Home', 'Switch Account']);
  });

  it('keeps the plain page-not-found page for unknown addresses', () => {
    const { text } = render(404, {}, null);
    expect(text).toContain('Page Not Found');
    expect(text).not.toContain('Private items');
  });
});
