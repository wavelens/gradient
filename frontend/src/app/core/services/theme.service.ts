/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable, signal, computed, Signal } from '@angular/core';

export type ThemePreference = 'light' | 'dark' | 'system';

const STORAGE_KEY = 'gradient.theme';

/// The stylesheet already follows the OS preference, so this only has to stamp an explicit override.
@Injectable({ providedIn: 'root' })
export class ThemeService {
  private pref = signal<ThemePreference>(this.restore());

  preference: Signal<ThemePreference> = this.pref.asReadonly();

  resolved: Signal<'light' | 'dark'> = computed(() => {
    const pref = this.pref();
    return pref === 'system' ? this.systemTheme() : pref;
  });

  constructor() {
    this.stamp(this.pref());
  }

  set(pref: ThemePreference): void {
    this.pref.set(pref);
    this.stamp(pref);
    try {
      if (pref === 'system') localStorage.removeItem(STORAGE_KEY);
      else localStorage.setItem(STORAGE_KEY, pref);
    } catch {
      // Storage can be denied in private windows; the in-memory preference still applies.
    }
  }

  private stamp(pref: ThemePreference): void {
    const root = document.documentElement;
    if (pref === 'system') root.removeAttribute('data-theme');
    else root.setAttribute('data-theme', pref);
  }

  private systemTheme(): 'light' | 'dark' {
    return window.matchMedia?.('(prefers-color-scheme: light)').matches ? 'light' : 'dark';
  }

  private restore(): ThemePreference {
    try {
      const stored = localStorage.getItem(STORAGE_KEY);
      return stored === 'light' || stored === 'dark' ? stored : 'system';
    } catch {
      return 'system';
    }
  }
}
