/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ComponentFixture, TestBed } from '@angular/core/testing';
import { EvalStatusBadgeComponent } from './eval-status-badge.component';

function render(status: string): ComponentFixture<EvalStatusBadgeComponent> {
  const fixture = TestBed.createComponent(EvalStatusBadgeComponent);
  fixture.componentRef.setInput('status', status);
  fixture.detectChanges();
  return fixture;
}

describe('EvalStatusBadgeComponent', () => {
  it('collapses Evaluating* statuses to a single "Evaluating" label', () => {
    const fixture = render('EvaluatingDerivation');
    expect(fixture.nativeElement.textContent.trim()).toContain('Evaluating');
  });

  it('colors the badge and its icon by phase', () => {
    const fixture = render('Completed');
    expect(fixture.nativeElement.querySelector('.eval-status-badge[data-phase="success"]')).toBeTruthy();
    expect(fixture.nativeElement.querySelector('gr-status-icon[data-phase="success"]')).toBeTruthy();
  });

  it('shows every active status as running', () => {
    for (const status of ['Fetching', 'EvaluatingFlake', 'Building']) {
      const fixture = render(status);
      expect(fixture.nativeElement.querySelector('gr-status-icon').dataset.phase).toBe('running');
    }
  });

  it('keeps queued distinct from running', () => {
    const fixture = render('Queued');
    expect(fixture.nativeElement.querySelector('gr-status-icon').dataset.phase).toBe('queued');
  });
});
