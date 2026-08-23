/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { MetricChartComponent } from './metric-chart.component';
import { MetricSeries } from './metric-chart.options';

@Component({
  standalone: true,
  imports: [MetricChartComponent],
  template: `
    <app-metric-chart
      [title]="title()"
      [bare]="bare()"
      type="line"
      [series]="series()"
      [categories]="['a', 'b']"
    ></app-metric-chart>
  `,
})
class HostComponent {
  title = signal('Build duration');
  bare = signal(false);
  series = signal<MetricSeries[]>([{ name: 'avg', data: [1, 2] }]);
}

async function render() {
  TestBed.configureTestingModule({ imports: [HostComponent] });
  const fixture = TestBed.createComponent(HostComponent);
  fixture.autoDetectChanges();
  await fixture.whenStable();
  return fixture;
}

describe('MetricChartComponent', () => {
  it('renders an echarts svg root into the plot host', async () => {
    const fixture = await render();
    expect(fixture.nativeElement.querySelector('.metric-chart__plot svg')).toBeTruthy();
  });

  it('shows the title as a heading by default', async () => {
    const fixture = await render();
    expect(fixture.nativeElement.querySelector('h3')?.textContent).toContain('Build duration');
  });

  it('drops the heading and card chrome when bare', async () => {
    const fixture = await render();
    fixture.componentInstance.bare.set(true);
    await fixture.whenStable();
    expect(fixture.nativeElement.querySelector('h3')).toBeNull();
    expect(fixture.nativeElement.querySelector('.metric-chart--bare')).toBeTruthy();
  });

  it('re-renders when the series input changes', async () => {
    const fixture = await render();
    const before = fixture.nativeElement.querySelector('.metric-chart__plot').innerHTML;
    fixture.componentInstance.series.set([{ name: 'avg', data: [90, 91] }]);
    await fixture.whenStable();
    expect(fixture.nativeElement.querySelector('.metric-chart__plot').innerHTML).not.toBe(before);
  });

  it('disposes the chart instance on destroy', async () => {
    const fixture = await render();
    fixture.destroy();
    expect(fixture.nativeElement.querySelector('.metric-chart__plot svg')).toBeNull();
  });
});
