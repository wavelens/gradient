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

  it('marks completed evaluations with the success class', () => {
    const fixture = render('Completed');
    expect(fixture.nativeElement.querySelector('.eval-status-badge.status-success')).toBeTruthy();
  });

  it('spins the icon for an actively running status', () => {
    const fixture = render('Building');
    expect(fixture.nativeElement.querySelector('.material-symbols-outlined.spinning')).toBeTruthy();
  });

  it('pulses (not spins) the icon while queued', () => {
    const fixture = render('Queued');
    expect(fixture.nativeElement.querySelector('.material-symbols-outlined.pulsing')).toBeTruthy();
    expect(fixture.nativeElement.querySelector('.material-symbols-outlined.spinning')).toBeNull();
  });
});
