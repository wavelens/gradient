/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, Input } from '@angular/core';
import { CommonModule } from '@angular/common';
import { NgApexchartsModule, ApexAxisChartSeries } from 'ng-apexcharts';

/// Thin dark-themed ApexCharts wrapper bound to the Job Board query API. Pass a
/// chart `type` (area/bar/line/radar/heatmap), `series`, and x-axis `categories`.
@Component({
  selector: 'app-metric-chart',
  standalone: true,
  imports: [CommonModule, NgApexchartsModule],
  template: `
    <div class="metric-chart">
      @if (title) {
        <h3>{{ title }}</h3>
      }
      <apx-chart
        [series]="series"
        [chart]="{ type: type, height: height, background: 'transparent', toolbar: { show: false }, animations: { enabled: false }, zoom: { allowMouseWheelZoom: false } }"
        [theme]="{ mode: 'dark' }"
        [plotOptions]="{ bar: { horizontal: horizontal } }"
        [xaxis]="{ categories: categories, labels: { rotate: -45, style: { colors: '#abb0b4', fontSize: '10px' } } }"
        [yaxis]="{ labels: { style: { colors: '#abb0b4' } } }"
        [dataLabels]="{ enabled: false }"
        [stroke]="{ curve: 'smooth', width: 2 }"
        [fill]="{ type: type === 'area' ? 'gradient' : 'solid', gradient: { shadeIntensity: 0.4, opacityFrom: 0.5, opacityTo: 0.05 } }"
        [grid]="{ borderColor: '#2d333b' }"
        [colors]="colors"
        [legend]="{ labels: { colors: '#abb0b4' } }"
      ></apx-chart>
    </div>
  `,
  styles: [
    `
      .metric-chart {
        background: #21262d;
        border: 1px solid #2d333b;
        border-radius: 8px;
        padding: 1rem;
      }
      h3 {
        color: #fff;
        font-size: 0.9rem;
        margin: 0 0 0.5rem;
      }
    `,
  ],
})
export class MetricChartComponent {
  @Input() title = '';
  @Input() type: 'area' | 'bar' | 'line' | 'radar' | 'heatmap' | 'donut' = 'area';
  @Input() height = 260;
  @Input() horizontal = false;
  @Input() series: ApexAxisChartSeries = [];
  @Input() categories: string[] = [];
  @Input() colors: string[] = ['#17a2b8', '#dc3545', '#28a745', '#fd7e14', '#6f42c1', '#e83e8c'];
}
