/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { type ThunkProgress, thunkProgress } from '@shared/evaluation';
import { ThunkProgressComponent } from './thunk-progress.component';

function renderThunkProgressComponentWithValueAndGetRootElement(progress: ThunkProgress): HTMLElement {
  const fixture = TestBed.createComponent(ThunkProgressComponent);
  fixture.componentRef.setInput('progress', progress);
  fixture.detectChanges();
  return fixture.nativeElement as HTMLElement;
}

describe('ThunkProgressComponent', () => {
  it('report evaluated thunks to screen readers and as visible label text', () => {
    const progress = thunkProgress({ kind: 'evaluating', thunks: 300 }, 1200);
    const root = renderThunkProgressComponentWithValueAndGetRootElement(progress!);

    const bar = root.querySelector('[role="progressbar"]');
    expect(bar?.getAttribute('aria-valuenow')).toBe('25');
    expect(bar?.getAttribute('aria-valuetext')).toBe(progress!.label);
    expect(root.querySelector('.thunk-label')?.textContent).toBe(progress!.label);
    expect(root.querySelectorAll('.seg')).toHaveLength(2);
  });
});
