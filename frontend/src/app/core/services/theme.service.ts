/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable, signal, computed, Signal } from '@angular/core';

export type ThemePreference = 'light' | 'dark' | 'system';

const STORAGE_KEY = 'gradient.theme';

/// Dark is the default. Light and system are opt-in, and the resolved theme is always stamped.
@Injectable({ providedIn: 'root' })
export class ThemeService {
  private pref = signal<ThemePreference>(this.restore());

  preference: Signal<ThemePreference> = this.pref.asReadonly();

  resolved: Signal<'light' | 'dark'> = computed(() => {
    const pref = this.pref();
    return pref === 'system' ? this.systemTheme() : pref;
  });

  constructor() {
    this.stamp(this.resolved());
  }

  set(pref: ThemePreference): void {
    this.pref.set(pref);
    this.stamp(this.resolved());
    try {
      if (pref === 'dark') localStorage.removeItem(STORAGE_KEY);
      else localStorage.setItem(STORAGE_KEY, pref);
    } catch {
      // Storage can be denied in private windows; the in-memory preference still applies.
    }
  }

  private stamp(theme: 'light' | 'dark'): void {
    document.documentElement.setAttribute('data-theme', theme);
  }

  private systemTheme(): 'light' | 'dark' {
    return window.matchMedia?.('(prefers-color-scheme: light)').matches ? 'light' : 'dark';
  }

  private restore(): ThemePreference {
    try {
      const stored = localStorage.getItem(STORAGE_KEY);
      return stored === 'light' || stored === 'system' ? stored : 'dark';
    } catch {
      return 'dark';
    }
  }
}
