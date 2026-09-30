/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import { AuthService } from '@core/services/auth.service';
import { BoardLayoutComponent } from './board-layout.component';

function tabs(superuser: boolean): string[] {
  TestBed.configureTestingModule({
    imports: [BoardLayoutComponent],
    providers: [provideRouter([]), { provide: AuthService, useValue: { user: signal({ superuser }) } }],
  });
  const fixture = TestBed.createComponent(BoardLayoutComponent);
  fixture.detectChanges();
  return Array.from((fixture.nativeElement as HTMLElement).querySelectorAll('.board-nav a')).map((a) => a.textContent!.trim());
}

describe('BoardLayoutComponent', () => {
  it('offers System Health to a superuser', () => {
    expect(tabs(true)).toContain('System Health');
  });

  it('places Storage right after Cache', () => {
    const shown = tabs(true);
    expect(shown[shown.indexOf('Cache') + 1]).toBe('Storage');
  });

  /// The health endpoint answers superusers only; a tab that can only fail is gone.
  it('hides System Health from everyone else', () => {
    const shown = tabs(false);
    expect(shown).not.toContain('System Health');
    expect(shown).not.toContain('Storage');
    expect(shown).toContain('Workers');
  });
});
