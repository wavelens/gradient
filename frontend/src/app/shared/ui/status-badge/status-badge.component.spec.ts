/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { StatusBadgeComponent } from './status-badge.component';

describe('StatusBadgeComponent', () => {
  it('colours the badge and its icon by phase and shows the label', () => {
    const fixture = TestBed.createComponent(StatusBadgeComponent);
    fixture.componentRef.setInput('phase', 'failure');
    fixture.componentRef.setInput('label', 'Failed');
    fixture.detectChanges();

    const badge = (fixture.nativeElement as HTMLElement).querySelector('.status-badge');
    expect(badge?.getAttribute('data-phase')).toBe('failure');
    expect(badge?.querySelector('gr-status-icon')?.getAttribute('data-phase')).toBe('failure');
    expect(badge?.textContent?.trim()).toBe('Failed');
  });
});
