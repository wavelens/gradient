/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { ActivatedRoute, convertToParamMap, provideRouter, Router } from '@angular/router';
import { of } from 'rxjs';
import { AuthService } from '@core/services/auth.service';
import { PendingUserDecisionsService } from '@core/services/pending-invites.service';
import { ErrorPageComponent } from './error-page.component';

function render(
  code: number,
  query: Record<string, string>,
  user: { username: string; superuser?: boolean } | null,
  pendingInvites = 0,
  otherPendingRequests = 0,
) {
  const logout = vi.fn(() => of(null));
  const refreshInvites = vi.fn();
  TestBed.configureTestingModule({
    imports: [ErrorPageComponent],
    providers: [
      provideRouter([]),
      { provide: AuthService, useValue: { user: signal(user), logout } },
      { provide: PendingUserDecisionsService, useValue: { invites: () => Array.from({ length: pendingInvites }), count: () => pendingInvites + otherPendingRequests, refresh: refreshInvites } },
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
  return { text: element.textContent ?? '', links, logout, fixture, refreshInvites };
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

  it('leaves out Review Invitations when only approval requests wait, since they grant no access', () => {
    const { links } = render(404, DENIED, { username: 'carol' }, 0, 3);
    expect(links.map((l) => l.text)).not.toContain('Review Invitations');
  });

  it('explains admin-only pages instead of redirecting', () => {
    const { text, links } = render(403, { from: '/admin/webhooks', only: 'admin' }, { username: 'bob' });
    expect(text).toContain('Admin Only');
    expect(text).toContain('Signed in as bob.');
    expect(links.map((l) => l.text)).toEqual(['Go Home', 'Switch Account']);
  });

  it('sends a refused board tab back to the job board', () => {
    const { text, links } = render(403, { from: '/board/workers', only: 'admin' }, { username: 'bob' });
    expect(text).toContain('Admin Only');
    expect(links).toContainEqual({ text: 'Back to Job Board', href: '/board/overview' });
    expect(links.map((l) => l.text)).not.toContain('Go Home');
  });

  it('keeps a refusal without the admin-only mark neutral', () => {
    const signedIn = render(403, { from: '/projects' }, { username: 'bob' });
    expect(signedIn.text).toContain('Access Denied');
    expect(signedIn.text).toContain("You don't have access to this page.");
    expect(signedIn.text).not.toContain('Admin');
    TestBed.resetTestingModule();
    const anonymous = render(403, { from: '/projects' }, null);
    expect(anonymous.text).toContain('Sign in to open this page.');
    expect(anonymous.text).not.toContain('Signed in as');
  });

  it('gives an anonymous visitor a single reason on a denied page', () => {
    const { text } = render(404, DENIED, null);
    expect(text).toContain("It doesn't exist, or it's private and needs sign-in.");
    expect(text).not.toContain("you don't have access");
    expect(text).not.toContain('Private items');
  });

  it('names the parent project on the back button', () => {
    const { links } = render(404, { kind: 'task', name: 'hosts', back: '/project/infra', from: '/project/infra/t/hosts' }, { username: 'carol' });
    expect(links).toContainEqual({ text: 'Back to Project', href: '/project/infra' });
  });

  it('names an item without a readable name by its kind only', () => {
    const { text, links } = render(404, { kind: 'execJob', back: '/projects', from: '/project/infra/task/hosts/exec/[uuid1]' }, { username: 'carol' });
    expect(text).toContain("Exec job not found. It doesn't exist, or you don't have access to it.");
    expect(links).toContainEqual({ text: 'Back to Projects', href: '/projects' });
  });

  it('gives a superuser the same denied page as any signed-in user', () => {
    const { text, links } = render(404, DENIED, { username: 'root', superuser: true });
    expect(text).toContain('Not Found or Access Denied');
    expect(text).toContain("don't have access");
    expect(links.map((l) => l.text)).toContain('Switch Account');
  });

  it('reloads pending invitations when a signed-in user lands on a denied page', () => {
    expect(render(404, DENIED, { username: 'carol' }).refreshInvites).toHaveBeenCalledOnce();
    TestBed.resetTestingModule();
    expect(render(404, DENIED, null).refreshInvites).not.toHaveBeenCalled();
  });

  it('sends Try Again back to the failed page, never reloading the error page', () => {
    const { fixture } = render(503, { from: '/caches' }, null);
    const navigate = vi.spyOn(TestBed.inject(Router), 'navigateByUrl').mockResolvedValue(true);
    fixture.componentInstance.retry();
    expect(navigate).toHaveBeenCalledWith('/caches');

    TestBed.resetTestingModule();
    const typed = render(503, {}, null);
    const home = vi.spyOn(TestBed.inject(Router), 'navigateByUrl').mockResolvedValue(true);
    typed.fixture.componentInstance.retry();
    expect(home).toHaveBeenCalledWith('/');
  });

  it('keeps the plain page-not-found page for unknown addresses', () => {
    const { text } = render(404, {}, null);
    expect(text).toContain('Page Not Found');
    expect(text).not.toContain('Private items');
  });
});
