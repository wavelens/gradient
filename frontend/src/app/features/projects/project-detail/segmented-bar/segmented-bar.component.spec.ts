/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ComponentFixture, TestBed } from '@angular/core/testing';
import { SegmentedBarComponent } from './segmented-bar.component';
import { BuildStatusCounts } from '@core/models';

function counts(p: Partial<BuildStatusCounts>): BuildStatusCounts {
  return { completed: 0, failed: 0, building: 0, queued: 0, substituted: 0, aborted: 0, ...p };
}

describe('SegmentedBarComponent', () => {
  let fixture: ComponentFixture<SegmentedBarComponent>;

  beforeEach(async () => {
    await TestBed.configureTestingModule({ imports: [SegmentedBarComponent] }).compileComponents();
    fixture = TestBed.createComponent(SegmentedBarComponent);
  });

  it('renders four proportional segments excluding substituted/aborted', () => {
    fixture.componentRef.setInput('counts', counts({ completed: 3, failed: 1, substituted: 9000, aborted: 5 }));
    fixture.detectChanges();
    const segs = fixture.componentInstance.segments();
    expect(segs.map(s => s.key)).toEqual(['completed', 'failed']);
    expect(segs.find(s => s.key === 'completed')!.pct).toBeCloseTo(75, 0);
    expect(segs.find(s => s.key === 'failed')!.pct).toBeCloseTo(25, 0);
    expect(fixture.componentInstance.isEmpty()).toBe(false);
  });

  it('reports empty when the four-segment total is zero', () => {
    fixture.componentRef.setInput('counts', counts({ substituted: 100 }));
    fixture.detectChanges();
    expect(fixture.componentInstance.isEmpty()).toBe(true);
  });
});
