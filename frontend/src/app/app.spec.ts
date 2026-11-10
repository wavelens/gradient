/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { Component } from '@angular/core';
import { Router, provideRouter } from '@angular/router';
import { provideHttpClient } from '@angular/common/http';
import { provideHttpClientTesting } from '@angular/common/http/testing';
import { Subject } from 'rxjs';
import { App } from './app';
import { ConfigService } from '@core/services/config.service';

@Component({ template: 'page' })
class PageStub {}

let slowGuard = new Subject<boolean>();

describe('App', () => {
  beforeEach(async () => {
    await TestBed.configureTestingModule({
      imports: [App],
      providers: [
        provideRouter([
          { path: '', component: PageStub },
          { path: 'bare', component: PageStub, data: { hideFooter: true } },
          { path: 'slow', component: PageStub, canActivate: [() => slowGuard] },
        ]),
        provideHttpClient(),
        provideHttpClientTesting(),
      ],
    }).compileComponents();
  });

  it('renders', () => {
    const fixture = TestBed.createComponent(App);
    expect(fixture.componentInstance).toBeTruthy();
  });

  it('shows the footer only once the first page has rendered, and not on pages that hide it', async () => {
    const fixture = TestBed.createComponent(App);
    fixture.detectChanges();
    const footer = () => fixture.nativeElement.querySelector('gr-footer');
    expect(footer()).toBeNull();

    await TestBed.inject(Router).navigateByUrl('/');
    fixture.detectChanges();
    expect(footer()).not.toBeNull();

    await TestBed.inject(Router).navigateByUrl('/bare');
    fixture.detectChanges();
    expect(footer()).toBeNull();
  });

  it('render pages into the single main landmark for skip link focus', async () => {
    const fixture = TestBed.createComponent(App);
    await TestBed.inject(Router).navigateByUrl('/');
    fixture.detectChanges();
    const root = fixture.nativeElement as HTMLElement;
    const mains = root.querySelectorAll('main');
    expect(mains).toHaveLength(1);
    expect(mains[0].textContent).toContain('page');
    const skip = root.querySelector<HTMLAnchorElement>('.gr-skip-link')!;
    const event = new MouseEvent('click', { bubbles: true, cancelable: true });
    skip.dispatchEvent(event);
    expect(event.defaultPrevented).toBe(true);
    expect(document.activeElement).toBe(mains[0]);
  });

  it('moves focus to main after each client navigation but not on the first page load', async () => {
    const fixture = TestBed.createComponent(App);
    const router = TestBed.inject(Router);
    const main = () => fixture.nativeElement.querySelector('main') as HTMLElement;
    const outside = document.createElement('button');
    document.body.appendChild(outside);
    outside.focus();

    await router.navigateByUrl('/');
    await fixture.whenStable();
    expect(document.activeElement).toBe(outside);

    await router.navigateByUrl('/bare');
    await fixture.whenStable();
    expect(document.activeElement).toBe(main());
    outside.remove();
  });

  it('leaves focus inside the page when the page itself kept or set it', async () => {
    const fixture = TestBed.createComponent(App);
    const router = TestBed.inject(Router);
    await router.navigateByUrl('/');
    await fixture.whenStable();
    const field = document.createElement('input');
    fixture.nativeElement.querySelector('main').appendChild(field);
    field.focus();

    await router.navigateByUrl('/bare');
    await fixture.whenStable();
    expect(document.activeElement).toBe(field);
  });

  it('marks the main area busy and shows a spinner while the first page waits for its data', async () => {
    vi.useFakeTimers();
    slowGuard = new Subject<boolean>();
    const fixture = TestBed.createComponent(App);
    const main = () => fixture.nativeElement.querySelector('main') as HTMLElement;
    const navigation = TestBed.inject(Router).navigateByUrl('/slow');
    await vi.advanceTimersByTimeAsync(100);
    fixture.detectChanges();
    expect(main().querySelector('gr-loading-spinner')).toBeNull();

    await vi.advanceTimersByTimeAsync(300);
    fixture.detectChanges();
    expect(main().getAttribute('aria-busy')).toBe('true');
    expect(main().textContent).toContain('Loading page...');

    slowGuard.next(true);
    slowGuard.complete();
    await navigation;
    fixture.detectChanges();
    expect(main().getAttribute('aria-busy')).toBeNull();
    expect(main().querySelector('gr-loading-spinner')).toBeNull();
    expect(main().textContent).toContain('page');
    vi.useRealTimers();
  });

  it('names the deployed commit next to the version in the footer', async () => {
    Object.assign(TestBed.inject(ConfigService), { backendVersion: '2.0.1', backendCommit: '455a55591f5f49fe6251d936cdc94d44a7c87df5' });
    const fixture = TestBed.createComponent(App);
    await TestBed.inject(Router).navigateByUrl('/');
    fixture.detectChanges();
    expect(fixture.nativeElement.querySelector('gr-footer').textContent).toContain('v2.0.1 · 455a5559');
  });
});
