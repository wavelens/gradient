/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { ThemeService } from './theme.service';

type MediaListener = (event: { matches: boolean }) => void;

let listener: MediaListener | null = null;

function stubPrefersLight(matches: boolean): void {
  listener = null;
  window.matchMedia = ((query: string) => ({
    matches,
    media: query,
    onchange: null,
    addEventListener: (_: string, fn: MediaListener) => {
      listener = fn;
    },
    removeEventListener: () => undefined,
    addListener: () => undefined,
    removeListener: () => undefined,
    dispatchEvent: () => false,
  })) as unknown as typeof window.matchMedia;
}

describe('ThemeService', () => {
  beforeEach(() => {
    localStorage.clear();
    document.documentElement.removeAttribute('data-theme');
    stubPrefersLight(false);
    TestBed.configureTestingModule({});
  });

  it('defaults to system and stamps no attribute', () => {
    const svc = TestBed.inject(ThemeService);
    expect(svc.preference()).toBe('system');
    expect(document.documentElement.hasAttribute('data-theme')).toBe(false);
  });

  it('resolves system to dark when the OS does not prefer light', () => {
    expect(TestBed.inject(ThemeService).resolved()).toBe('dark');
  });

  it('resolves system to light when the OS prefers light', () => {
    stubPrefersLight(true);
    expect(TestBed.inject(ThemeService).resolved()).toBe('light');
  });

  it('follows a live OS change while the preference is system', () => {
    const svc = TestBed.inject(ThemeService);
    expect(svc.resolved()).toBe('dark');
    listener?.({ matches: true });
    expect(svc.resolved()).toBe('light');
  });

  it('ignores a live OS change once a theme is chosen explicitly', () => {
    const svc = TestBed.inject(ThemeService);
    svc.set('dark');
    listener?.({ matches: true });
    expect(svc.resolved()).toBe('dark');
  });

  it('stamps data-theme for an explicit choice', () => {
    const svc = TestBed.inject(ThemeService);
    svc.set('light');
    expect(document.documentElement.getAttribute('data-theme')).toBe('light');
    expect(svc.resolved()).toBe('light');
  });

  it('persists the choice and restores it', () => {
    TestBed.inject(ThemeService).set('light');
    TestBed.resetTestingModule();
    TestBed.configureTestingModule({});
    expect(TestBed.inject(ThemeService).preference()).toBe('light');
  });

  it('clears the attribute when returning to system', () => {
    const svc = TestBed.inject(ThemeService);
    svc.set('dark');
    svc.set('system');
    expect(document.documentElement.hasAttribute('data-theme')).toBe(false);
  });
});
