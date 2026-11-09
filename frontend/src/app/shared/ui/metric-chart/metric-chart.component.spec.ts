/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import * as echarts from 'echarts/core';
import { MetricChartComponent, resolveChartTheme } from './metric-chart.component';
import { MetricSeries } from './metric-chart.options';

@Component({
  standalone: true,
  imports: [MetricChartComponent],
  template: `
    <gr-metric-chart
      [title]="title()"
      [bare]="bare()"
      type="line"
      [series]="series()"
      [categories]="['a', 'b']"
    ></gr-metric-chart>
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
    expect(fixture.nativeElement.querySelector('h2')?.textContent).toContain('Build duration');
  });

  it('drops the heading and card chrome when bare', async () => {
    const fixture = await render();
    fixture.componentInstance.bare.set(true);
    await fixture.whenStable();
    expect(fixture.nativeElement.querySelector('h2')).toBeNull();
    expect(fixture.nativeElement.querySelector('.metric-chart--bare')).toBeTruthy();
  });

  it('re-renders when the series input changes', async () => {
    const fixture = await render();
    const before = fixture.nativeElement.querySelector('.metric-chart__plot').innerHTML;
    fixture.componentInstance.series.set([{ name: 'avg', data: [90, 91] }]);
    await fixture.whenStable();
    expect(fixture.nativeElement.querySelector('.metric-chart__plot').innerHTML).not.toBe(before);
  });

  it('says No data over a plot whose series hold no points', async () => {
    const fixture = await render();
    expect(fixture.nativeElement.querySelector('.metric-chart__empty')).toBeNull();
    fixture.componentInstance.series.set([{ name: 'avg', data: [] }, { name: 'max', data: [null, null] }]);
    await fixture.whenStable();
    expect(fixture.nativeElement.querySelector('.metric-chart__empty')?.textContent).toContain('No data');
  });

  it('says Couldn\'t load instead of No data when the data failed to load', async () => {
    const fixture = TestBed.createComponent(MetricChartComponent);
    fixture.componentRef.setInput('error', true);
    fixture.autoDetectChanges();
    await fixture.whenStable();
    const root = fixture.nativeElement as HTMLElement;
    expect(root.querySelector('.metric-chart__error')?.textContent).toContain("Couldn't load");
    expect(root.textContent).not.toContain('No data');

    fixture.componentRef.setInput('loading', true);
    await fixture.whenStable();
    expect(root.querySelector('.metric-chart__error')).toBeNull();
  });

  it('waits for the plot to get a size before it starts the chart', async () => {
    let notify: (width: number, height: number) => void = () => undefined;
    vi.stubGlobal('ResizeObserver', class {
      constructor(callback: (entries: { contentRect: { width: number; height: number } }[]) => void) {
        notify = (width, height) => callback([{ contentRect: { width, height } }]);
      }
      observe(): void { /* driven by notify */ }
      disconnect(): void { /* nothing to release */ }
    });
    try {
      const fixture = await render();
      const plot = () => fixture.nativeElement.querySelector('.metric-chart__plot svg');
      expect(plot()).toBeNull();
      notify(0, 0);
      expect(plot()).toBeNull();
      notify(400, 260);
      expect(plot()).toBeTruthy();
    } finally {
      vi.unstubAllGlobals();
    }
  });

  it('drops the radar when the chart switches to bars', async () => {
    const fixture = TestBed.createComponent(MetricChartComponent);
    fixture.componentRef.setInput('type', 'radar');
    fixture.componentRef.setInput('categories', ['x86_64', 'aarch64', 'riscv64']);
    fixture.componentRef.setInput('series', [{ name: 'builds', data: [3, 2, 1] }]);
    fixture.autoDetectChanges();
    await fixture.whenStable();
    const plot = fixture.nativeElement.querySelector('.metric-chart__plot') as HTMLElement;
    const radarOf = () => (echarts.getInstanceByDom(plot)?.getOption() as { radar?: unknown[] }).radar ?? [];
    expect(radarOf().length).toBe(1);

    fixture.componentRef.setInput('type', 'bar');
    await fixture.whenStable();
    expect(radarOf().length).toBe(0);
  });

  it('disposes the chart instance on destroy', async () => {
    const fixture = await render();
    fixture.destroy();
    expect(fixture.nativeElement.querySelector('.metric-chart__plot svg')).toBeNull();
  });
  it('renders a subtitle under the title', async () => {
    const fixture = TestBed.createComponent(MetricChartComponent);
    fixture.componentRef.setInput('title', 'Total Build Time');
    fixture.componentRef.setInput('subtitle', 'Sum of all completed build durations');
    fixture.detectChanges();
    await fixture.whenStable();
    const root = fixture.nativeElement as HTMLElement;
    expect(root.querySelector('h2')?.textContent).toContain('Total Build Time');
    expect(root.querySelector('.metric-chart__subtitle')?.textContent).toContain('Sum of all');
  });

  it('covers the plot with a placeholder while loading and keeps the header', async () => {
    const fixture = TestBed.createComponent(MetricChartComponent);
    fixture.componentRef.setInput('title', 'Traffic over time');
    fixture.componentRef.setInput('loading', true);
    fixture.detectChanges();
    await fixture.whenStable();
    const root = fixture.nativeElement as HTMLElement;
    expect(root.querySelector('h2')?.textContent).toContain('Traffic over time');
    expect(root.querySelector('.metric-chart')?.getAttribute('aria-busy')).toBe('true');
    expect(root.querySelector('gr-skeleton')).not.toBeNull();

    fixture.componentRef.setInput('loading', false);
    fixture.detectChanges();
    expect(root.querySelector('gr-skeleton')).toBeNull();
  });

  it('drops the header entirely when bare', async () => {
    const fixture = TestBed.createComponent(MetricChartComponent);
    fixture.componentRef.setInput('title', 'Total');
    fixture.componentRef.setInput('subtitle', 'Sum');
    fixture.componentRef.setInput('bare', true);
    fixture.detectChanges();
    await fixture.whenStable();
    const root = fixture.nativeElement as HTMLElement;
    expect(root.querySelector('h2')).toBeNull();
    expect(root.querySelector('.metric-chart__subtitle')).toBeNull();
  });
  it('gives up to eight series distinct palette colours and keeps the danger colour out of it', () => {
    const roles = ['--gr-graph-running', '--gr-graph-success', '--gr-graph-warning', '--gr-chart-purple', '--gr-chart-cyan', '--gr-chart-pink', '--gr-chart-orange', '--gr-chart-teal'];
    const root = document.documentElement.style;
    roles.forEach((role, i) => root.setProperty(role, `#00000${i}`));
    root.setProperty('--gr-graph-danger', '#ff0000');
    try {
      const palette = resolveChartTheme().palette;
      expect(palette.length).toBe(8);
      expect(new Set(palette).size).toBe(8);
      expect(palette).not.toContain('#ff0000');
    } finally {
      [...roles, '--gr-graph-danger'].forEach((role) => root.removeProperty(role));
    }
  });

  it('projects a control into the chart header', async () => {
    TestBed.configureTestingModule({ imports: [ActionHost] });
    const fixture = TestBed.createComponent(ActionHost);
    fixture.detectChanges();
    await fixture.whenStable();
    const root = fixture.nativeElement as HTMLElement;
    expect(root.querySelector('.metric-chart__actions button')?.textContent).toContain('Hours');
  });
});

@Component({
  standalone: true,
  imports: [MetricChartComponent],
  template: `
    <gr-metric-chart title="Storage">
      <button slot="actions">Hours</button>
    </gr-metric-chart>
  `,
})
class ActionHost {}

