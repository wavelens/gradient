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
import { buildClosureSankey, SankeyNode, SankeyLink } from './closure-aggregate';

const TOP_N = 500;
const SVG_NS = 'http://www.w3.org/2000/svg';
const ACCENT = '#fd7e14';
const OTHERS_FILL = '#4b5563';

type LaidNode = SankeyNode & { x0: number; x1: number; y0: number; y1: number; depth: number };

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
    const model = buildClosureSankey(graph, TOP_N);

    const { sankey, sankeyLinkHorizontal, sankeyJustify } = await import('d3-sankey');

    this.zone.runOutsideAngular(() => {
      try {
        const containerWidth = el.clientWidth || 1200;
        const height = Math.max(420, model.nodes.length * 11);

        const layout = (width: number) =>
          sankey<SankeyNode, SankeyLink>()
            .nodeId((d) => d.id)
            .nodeWidth(14)
            .nodePadding(6)
            .nodeAlign(sankeyJustify)
            .extent([[1, 6], [width - 1, height - 6]])({
            nodes: model.nodes.map((n) => ({ ...n })),
            links: model.links.map((l) => ({ ...l, value: Math.max(1, l.value) })),
          });

        // First pass derives the column count; widen so columns stay legible.
        const probe = layout(containerWidth);
        const columns = Math.max(...probe.nodes.map((n) => (n.depth ?? 0))) + 1;
        const width = Math.max(containerWidth, columns * 220);
        const { nodes, links } = layout(width);

        el.innerHTML = '';
        const linkPath = sankeyLinkHorizontal() as unknown as (l: unknown) => string;
        el.appendChild(this.draw(nodes as LaidNode[], links as never, width, height, linkPath));
      } catch {
        this.zone.run(() => this.errorMsg.set('Unable to render closure diagram'));
      }
    });
  }

  private draw(
    nodes: LaidNode[],
    links: { source: LaidNode; target: LaidNode; width?: number; value: number }[],
    width: number,
    height: number,
    linkPath: (l: unknown) => string,
  ): SVGSVGElement {
    const svg = document.createElementNS(SVG_NS, 'svg');
    svg.setAttribute('width', `${width}`);
    svg.setAttribute('height', `${height}`);
    svg.setAttribute('viewBox', `0 0 ${width} ${height}`);

    const linkLayer = document.createElementNS(SVG_NS, 'g');
    linkLayer.setAttribute('fill', 'none');
    for (const l of links) {
      const path = document.createElementNS(SVG_NS, 'path');
      path.setAttribute('d', linkPath(l));
      path.setAttribute('stroke', this.fill(l.source));
      path.setAttribute('stroke-width', `${Math.max(1, l.width ?? 1)}`);
      path.setAttribute('stroke-opacity', '0.35');
      path.appendChild(this.titleEl(`${l.source.name} → ${l.target.name}\n${this.formatBytes(l.value)}`));
      linkLayer.appendChild(path);
    }
    svg.appendChild(linkLayer);

    const nodeLayer = document.createElementNS(SVG_NS, 'g');
    for (const n of nodes) {
      const rect = document.createElementNS(SVG_NS, 'rect');
      rect.setAttribute('x', `${n.x0}`);
      rect.setAttribute('y', `${n.y0}`);
      rect.setAttribute('width', `${n.x1 - n.x0}`);
      rect.setAttribute('height', `${Math.max(1, n.y1 - n.y0)}`);
      rect.setAttribute('fill', this.fill(n));
      rect.setAttribute('rx', '1');
      rect.appendChild(this.titleEl(this.nodeTooltip(n)));
      nodeLayer.appendChild(rect);

      if (n.y1 - n.y0 < 9) continue; // skip labels on slivers to avoid overlap
      const text = document.createElementNS(SVG_NS, 'text');
      const leftHalf = n.x0 < width / 2;
      text.setAttribute('x', `${leftHalf ? n.x1 + 5 : n.x0 - 5}`);
      text.setAttribute('y', `${(n.y0 + n.y1) / 2}`);
      text.setAttribute('dy', '0.35em');
      text.setAttribute('text-anchor', leftHalf ? 'start' : 'end');
      text.setAttribute('fill', '#e5e7eb');
      text.setAttribute('font-size', '11');
      text.textContent = `${this.shortName(n.name)} · ${this.formatBytes(n.value)}`;
      nodeLayer.appendChild(text);
    }
    svg.appendChild(nodeLayer);
    return svg;
  }

  private fill(n: SankeyNode): string {
    return n.bucketedCount ? OTHERS_FILL : ACCENT;
  }

  private nodeTooltip(n: SankeyNode): string {
    if (n.bucketedCount) return `${n.name}\nclosure ${this.formatBytes(n.value)}`;
    return `${n.name}\nclosure ${this.formatBytes(n.value)} · own ${this.formatBytes(n.ownSize)}`;
  }

  private titleEl(text: string): SVGTitleElement {
    const title = document.createElementNS(SVG_NS, 'title');
    title.textContent = text;
    return title;
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
