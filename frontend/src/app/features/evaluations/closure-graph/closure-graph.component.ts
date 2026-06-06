/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import {
  Component, OnInit, OnDestroy, inject, signal, computed,
  ViewChild, ElementRef, NgZone, ChangeDetectorRef,
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
const MIN_SCALE = 0.02;
const MAX_SCALE = 4;

type LaidNode = SankeyNode & { x0: number; x1: number; y0: number; y1: number; depth: number };
type LaidLink = { source: LaidNode; target: LaidNode; width?: number; value: number };

@Component({
  selector: 'app-closure-graph',
  standalone: true,
  imports: [CommonModule, ButtonModule, LoadingSpinnerComponent],
  templateUrl: './closure-graph.component.html',
  styleUrl: './closure-graph.component.scss',
})
export class ClosureGraphComponent implements OnInit, OnDestroy {
  private route = inject(ActivatedRoute);
  private router = inject(Router);
  private evalService = inject(EvaluationsService);
  private zone = inject(NgZone);
  private cdr = inject(ChangeDetectorRef);

  @ViewChild('svgEl') svgRef!: ElementRef<SVGSVGElement>;
  @ViewChild('graphEl') graphRef!: ElementRef<SVGGElement>;

  loading = signal(true);
  errorMsg = signal<string | null>(null);
  truncated = signal(false);
  rootName = signal('');
  totalLabel = signal('');
  nodeCount = signal(0);

  scale = signal(1);
  tx = signal(0);
  ty = signal(0);
  transform = computed(() => `translate(${this.tx()},${this.ty()}) scale(${this.scale()})`);

  orgName = '';
  kind = '';
  id = '';
  closureType = 'runtime';
  typeLabel = signal('');

  private bounds = { minX: 0, minY: 0, maxX: 0, maxY: 0 };
  private isPanning = false;
  private panStart = { x: 0, y: 0, tx: 0, ty: 0 };

  ngOnInit(): void {
    document.documentElement.style.overflow = 'hidden';
    document.body.style.overflow = 'hidden';
    this.orgName = this.route.snapshot.paramMap.get('org') || '';
    this.kind = this.route.snapshot.paramMap.get('kind') || 'build';
    this.id = this.route.snapshot.paramMap.get('id') || '';
    this.closureType = this.route.snapshot.queryParamMap.get('type') === 'build' ? 'build' : 'runtime';
    this.typeLabel.set(this.closureType === 'build' ? 'Build closure' : 'Runtime closure');
    this.load();
  }

  ngOnDestroy(): void {
    document.documentElement.style.overflow = '';
    document.body.style.overflow = '';
  }

  private load(): void {
    const isEval = this.kind === 'eval';
    const req = this.closureType === 'build'
      ? (isEval ? this.evalService.getEvalClosure(this.id) : this.evalService.getBuildClosure(this.id))
      : (isEval ? this.evalService.getEvalRuntimeClosure(this.id) : this.evalService.getBuildRuntimeClosure(this.id));
    req.subscribe({
      next: (g) => {
        this.nodeCount.set(g.node_count);
        this.truncated.set(g.truncated);
        this.totalLabel.set(this.formatBytes(g.total_size_bytes ?? 0));
        const rootNode = g.nodes.find((n) => g.roots.includes(n.id));
        this.rootName.set(rootNode?.name ?? '');
        this.loading.set(false);
        this.cdr.detectChanges();
        setTimeout(() => this.render(g), 0);
      },
      error: () => {
        this.loading.set(false);
        this.errorMsg.set('Failed to load closure');
      },
    });
  }

  private async render(graph: ClosureGraph): Promise<void> {
    const group = this.graphRef?.nativeElement;
    const svg = this.svgRef?.nativeElement;
    if (!group || !svg || !graph.nodes.length) return;
    const model = buildClosureSankey(graph, TOP_N);

    const { sankey, sankeyLinkHorizontal, sankeyJustify } = await import('d3-sankey');

    this.zone.runOutsideAngular(() => {
      try {
        const probeWidth = svg.clientWidth || 1200;

        const layout = (width: number, height: number) =>
          sankey<SankeyNode, SankeyLink>()
            .nodeId((d) => d.id)
            .nodeWidth(14)
            .nodePadding(14)
            .nodeAlign(sankeyJustify)
            .extent([[1, 6], [width - 1, height - 6]])({
            nodes: model.nodes.map((n) => ({ ...n })),
            links: model.links.map((l) => ({ ...l, value: Math.max(1, l.value) })),
          });

        // Probe pass derives the column layout; size the canvas to the densest
        // column so every band gets room, and widen columns to stay legible.
        const probe = layout(probeWidth, 1000);
        const perDepth = new Map<number, number>();
        for (const n of probe.nodes) perDepth.set(n.depth ?? 0, (perDepth.get(n.depth ?? 0) ?? 0) + 1);
        const columns = Math.max(...perDepth.keys()) + 1;
        const densest = Math.max(...perDepth.values());
        const width = Math.max(probeWidth, columns * 240);
        const height = Math.max(480, densest * 28);
        const { nodes, links } = layout(width, height);

        const linkPath = sankeyLinkHorizontal() as unknown as (l: unknown) => string;
        this.build(group, nodes as LaidNode[], links as unknown as LaidLink[], width, linkPath);
        this.bounds = { minX: 0, minY: 0, maxX: width, maxY: height };
      } catch {
        this.zone.run(() => this.errorMsg.set('Unable to render closure diagram'));
        return;
      }
    });
    this.fitToScreen();
  }

  private build(
    group: SVGGElement,
    nodes: LaidNode[],
    links: LaidLink[],
    width: number,
    linkPath: (l: unknown) => string,
  ): void {
    group.innerHTML = '';

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
    group.appendChild(linkLayer);

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

      if (n.y1 - n.y0 < 3) continue; // skip labels on slivers to cut clutter
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
    group.appendChild(nodeLayer);
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

  onWheel(event: WheelEvent): void {
    event.preventDefault();
    const rect = this.svgRef.nativeElement.getBoundingClientRect();
    const mx = event.clientX - rect.left;
    const my = event.clientY - rect.top;
    this.zoomAt(mx, my, event.deltaY > 0 ? 0.9 : 1.1);
  }

  onSvgMousedown(event: MouseEvent): void {
    if (event.button !== 0) return;
    this.isPanning = true;
    this.panStart = { x: event.clientX, y: event.clientY, tx: this.tx(), ty: this.ty() };
  }

  onMousemove(event: MouseEvent): void {
    if (!this.isPanning) return;
    this.tx.set(this.panStart.tx + event.clientX - this.panStart.x);
    this.ty.set(this.panStart.ty + event.clientY - this.panStart.y);
  }

  onMouseup(): void {
    this.isPanning = false;
  }

  private zoomAt(cx: number, cy: number, factor: number): void {
    const oldScale = this.scale();
    const newScale = Math.max(MIN_SCALE, Math.min(MAX_SCALE, oldScale * factor));
    const eff = newScale / oldScale;
    if (eff === 1) return;
    this.scale.set(newScale);
    this.tx.update((v) => cx - eff * (cx - v));
    this.ty.update((v) => cy - eff * (cy - v));
  }

  zoomIn(): void {
    const rect = this.svgRef?.nativeElement.getBoundingClientRect();
    if (rect) this.zoomAt(rect.width / 2, rect.height / 2, 1.3);
  }

  zoomOut(): void {
    const rect = this.svgRef?.nativeElement.getBoundingClientRect();
    if (rect) this.zoomAt(rect.width / 2, rect.height / 2, 1 / 1.3);
  }

  fitToScreen(): void {
    const rect = this.svgRef?.nativeElement.getBoundingClientRect();
    if (!rect) return;
    const pad = 40;
    const gw = this.bounds.maxX - this.bounds.minX + pad * 2;
    const gh = this.bounds.maxY - this.bounds.minY + pad * 2;
    if (gw <= 0 || gh <= 0) return;
    const s = Math.min(rect.width / gw, rect.height / gh, 1.2);
    this.zone.run(() => {
      this.scale.set(s);
      this.tx.set(rect.width / 2 - ((this.bounds.minX + this.bounds.maxX) / 2) * s);
      this.ty.set(rect.height / 2 - ((this.bounds.minY + this.bounds.maxY) / 2) * s);
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
