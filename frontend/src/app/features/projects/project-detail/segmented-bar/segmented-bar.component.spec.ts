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

  it('renders all four segments proportionally excluding substituted/aborted, zero counts at 0% width', () => {
    fixture.componentRef.setInput('counts', counts({ completed: 3, failed: 1, substituted: 9000, aborted: 5 }));
    fixture.detectChanges();
    const segs = fixture.componentInstance.segments();
    expect(segs.map(s => s.key)).toEqual(['completed', 'failed', 'building', 'queued']);
    expect(segs.find(s => s.key === 'completed')!.pct).toBeCloseTo(75, 0);
    expect(segs.find(s => s.key === 'failed')!.pct).toBeCloseTo(25, 0);
    expect(segs.find(s => s.key === 'building')!.pct).toBe(0);
    expect(segs.find(s => s.key === 'queued')!.pct).toBe(0);
    expect(fixture.componentInstance.isEmpty()).toBe(false);
  });

  it('renders a single full green segment when work finished entirely via substitution', () => {
    fixture.componentRef.setInput('counts', counts({ substituted: 100 }));
    fixture.detectChanges();
    expect(fixture.componentInstance.allSubstituted()).toBe(true);
    expect(fixture.componentInstance.isEmpty()).toBe(false);
    const seg = fixture.nativeElement.querySelector('.seg-completed') as HTMLElement;
    expect(seg).toBeTruthy();
    expect(seg.style.width).toBe('100%');
  });

  it('reports empty when all counts are zero', () => {
    fixture.componentRef.setInput('counts', counts({}));
    fixture.detectChanges();
    expect(fixture.componentInstance.isEmpty()).toBe(true);
    expect(fixture.nativeElement.querySelector('.seg-empty')).toBeTruthy();
  });

  it('shows an instant custom tooltip with the hovered segment count', () => {
    fixture.componentRef.setInput('counts', counts({ completed: 3, failed: 1 }));
    fixture.detectChanges();
    const seg = fixture.nativeElement.querySelector('.seg-completed') as HTMLElement;
    seg.dispatchEvent(new MouseEvent('mouseenter'));
    fixture.detectChanges();
    const tip = fixture.nativeElement.querySelector('.tipbox') as HTMLElement;
    expect(tip?.textContent?.trim()).toBe('3 completed');
    fixture.nativeElement.querySelector('.segbar')!.dispatchEvent(new MouseEvent('mouseleave'));
    fixture.detectChanges();
    expect(fixture.nativeElement.querySelector('.tipbox')).toBeNull();
  });
});
