/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ComponentFixture, TestBed } from '@angular/core/testing';
import { SegmentedBarComponent } from './segmented-bar.component';
import { ratioSegments } from './ratio-segments';
import { BuildStatusCounts } from '@core/models';

function counts(p: Partial<BuildStatusCounts>): BuildStatusCounts {
  return { completed: 0, failed: 0, building: 0, queued: 0, substituted: 0, aborted: 0, dependency_failed: 0, ...p };
}

describe('SegmentedBarComponent', () => {
  let fixture: ComponentFixture<SegmentedBarComponent>;

  beforeEach(async () => {
    await TestBed.configureTestingModule({ imports: [SegmentedBarComponent] }).compileComponents();
    fixture = TestBed.createComponent(SegmentedBarComponent);
  });

  const widths = () => [...fixture.nativeElement.querySelectorAll('.seg')].map((s: HTMLElement) => [s.className.replace(' seg-pulsing', ''), s.style.width]);

  it('Draw all segments proportionally. Aborted segments use gray. No replacement build.', () => {
    fixture.componentRef.setInput('counts', counts({ completed: 3, failed: 1, substituted: 9000, aborted: 4 }));
    fixture.detectChanges();
    expect(widths()).toEqual([
      ['seg seg-completed', '37.5%'],
      ['seg seg-failed', '12.5%'],
      ['seg seg-aborted', '50%'],
      ['seg seg-building', '0%'],
      ['seg seg-queued', '0%'],
    ]);
  });

  it('names aborted and not built builds apart in the gray segment hint', () => {
    fixture.componentRef.setInput('counts', counts({ completed: 1, aborted: 2, dependency_failed: 1 }));
    fixture.detectChanges();
    expect(fixture.componentInstance.shown().find((s) => s.tone === 'aborted')?.label).toBe('2 aborted, 1 not built');
  });

  /// An endless animation on a segment without width cannot leave the main thread,
  /// and then repaints the page on every frame.
  it('pulses the building segment only while it has width', () => {
    const pulsing = () => fixture.nativeElement.querySelectorAll('.seg-pulsing').length;
    fixture.componentRef.setInput('counts', counts({ completed: 3 }));
    fixture.detectChanges();
    expect(pulsing()).toBe(0);

    fixture.componentRef.setInput('counts', counts({ completed: 3, building: 1 }));
    fixture.detectChanges();
    expect(pulsing()).toBe(1);
  });

  it('renders a single full green segment when work finished entirely via substitution', () => {
    fixture.componentRef.setInput('counts', counts({ substituted: 100 }));
    fixture.detectChanges();
    expect(widths()).toEqual([['seg seg-completed', '100%']]);
  });

  it('draws the empty track when all counts are zero', () => {
    fixture.componentRef.setInput('counts', counts({}));
    fixture.detectChanges();
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

  it('draws a byte download as a building part and an idle remainder without hover', () => {
    fixture.componentRef.setInput('segments', ratioSegments(512, 2048));
    fixture.detectChanges();
    expect(widths()).toEqual([['seg seg-building', '25%'], ['seg seg-queued', '75%']]);
    expect(fixture.nativeElement.querySelector('.segbar').classList).not.toContain('segbar--build-status-counts');
    fixture.nativeElement.querySelector('.seg-building').dispatchEvent(new MouseEvent('mouseenter'));
    fixture.detectChanges();
    expect(fixture.nativeElement.querySelector('.tipbox')).toBeNull();
  });

  it('fills a download of unknown size with one building segment', () => {
    fixture.componentRef.setInput('segments', ratioSegments(512, null));
    fixture.detectChanges();
    expect(widths()).toEqual([['seg seg-building', '100%']]);
  });
});
