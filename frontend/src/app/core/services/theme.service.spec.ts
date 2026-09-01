/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { ThemeService } from './theme.service';

function stubPrefersLight(matches: boolean): void {
  window.matchMedia = ((query: string) => ({
    matches,
    media: query,
    onchange: null,
    addEventListener: () => undefined,
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

  it('defaults to dark and stamps it', () => {
    const svc = TestBed.inject(ThemeService);
    expect(svc.preference()).toBe('dark');
    expect(svc.resolved()).toBe('dark');
    expect(document.documentElement.getAttribute('data-theme')).toBe('dark');
  });

  it('stays dark even when the OS prefers light', () => {
    stubPrefersLight(true);
    expect(TestBed.inject(ThemeService).resolved()).toBe('dark');
  });

  it('follows the OS only when system is chosen explicitly', () => {
    stubPrefersLight(true);
    const svc = TestBed.inject(ThemeService);
    svc.set('system');
    expect(svc.resolved()).toBe('light');
    expect(document.documentElement.getAttribute('data-theme')).toBe('light');
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

  it('returns to dark when the choice is cleared', () => {
    const svc = TestBed.inject(ThemeService);
    svc.set('light');
    svc.set('dark');
    expect(document.documentElement.getAttribute('data-theme')).toBe('dark');
    expect(localStorage.getItem('gradient.theme')).toBeNull();
  });
});
