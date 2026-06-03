/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import {
  Component, OnInit, inject, signal, ViewChild, ElementRef, NgZone,
} from '@angular/core';
import { CommonModule } from '@angular/common';
import { ActivatedRoute, Router } from '@angular/router';
import { ButtonModule } from 'primeng/button';
import { EvaluationsService, ClosureGraph } from '@core/services/evaluations.service';
import { LoadingSpinnerComponent } from '@shared/components/loading-spinner/loading-spinner.component';
import { aggregateTopN } from './closure-aggregate';

const TOP_N = 30;

@Component({
  selector: 'app-closure-graph',
  standalone: true,
  imports: [CommonModule, ButtonModule, LoadingSpinnerComponent],
  templateUrl: './closure-graph.component.html',
  styleUrl: './closure-graph.component.scss',
})
export class ClosureGraphComponent implements OnInit {
  private route = inject(ActivatedRoute);
  private router = inject(Router);
  private evalService = inject(EvaluationsService);
  private zone = inject(NgZone);

  @ViewChild('sankeyEl') sankeyRef!: ElementRef<HTMLDivElement>;

  loading = signal(true);
  errorMsg = signal<string | null>(null);
  truncated = signal(false);
  rootName = signal('');
  totalLabel = signal('');
  nodeCount = signal(0);

  orgName = '';
  kind = '';
  id = '';

  ngOnInit(): void {
    this.orgName = this.route.snapshot.paramMap.get('org') || '';
    this.kind = this.route.snapshot.paramMap.get('kind') || 'build';
    this.id = this.route.snapshot.paramMap.get('id') || '';
    this.load();
  }

  private load(): void {
    const req = this.kind === 'eval'
      ? this.evalService.getEvalClosure(this.id)
      : this.evalService.getBuildClosure(this.id);
    req.subscribe({
      next: (g) => {
        this.nodeCount.set(g.node_count);
        this.truncated.set(g.truncated);
        this.totalLabel.set(this.formatBytes(g.total_size_bytes ?? 0));
        const rootNode = g.nodes.find((n) => g.roots.includes(n.id));
        this.rootName.set(rootNode?.name ?? '');
        this.loading.set(false);
        setTimeout(() => this.render(g), 0);
      },
      error: () => {
        this.loading.set(false);
        this.errorMsg.set('Failed to load closure');
      },
    });
  }

  private async render(graph: ClosureGraph): Promise<void> {
    const el = this.sankeyRef?.nativeElement;
    if (!el || !graph.nodes.length) return;
    const agg = aggregateTopN(graph, TOP_N);
    const sizeById = new Map(agg.nodes.map((n) => [n.id, n.nar_size ?? 0]));

    const { default: ApexSankey } = await import('apexsankey');

    const nodes = agg.nodes.map((n) => ({
      id: n.id,
      title: `${this.shortName(n.name)} · ${this.formatBytes(n.nar_size ?? 0)}`,
    }));
    const edges = agg.edges.map((e) => ({
      source: e.source,
      target: e.target,
      value: Math.max(1, sizeById.get(e.source) ?? 1),
      type: 'closure',
    }));

    this.zone.runOutsideAngular(() => {
      el.innerHTML = '';
      try {
        const chart = new ApexSankey(el, {
          width: el.clientWidth || 1000,
          height: Math.max(420, agg.nodes.length * 24),
          nodeWidth: 16,
          fontColor: '#e5e7eb',
          canvasStyle: 'background:#0d1118;',
          tooltipTheme: 'dark',
        });
        chart.render({ nodes, edges, options: chart.options });
      } catch {
        this.zone.run(() => this.errorMsg.set('Unable to render closure diagram'));
      }
    });
  }

  private shortName(name: string): string {
    return name.length > 28 ? name.slice(0, 27) + '…' : name;
  }

  formatBytes(bytes: number): string {
    if (!bytes || bytes <= 0) return '0 B';
    const units = ['B', 'KB', 'MB', 'GB', 'TB'];
    const i = Math.floor(Math.log(bytes) / Math.log(1024));
    return `${(bytes / Math.pow(1024, i)).toFixed(1)} ${units[Math.min(i, units.length - 1)]}`;
  }

  goBack(): void {
    this.router.navigate(['/organization', this.orgName]);
  }
}
