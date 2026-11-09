/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import {
  Component,
  OnInit,
  OnDestroy,
  inject,
  signal,
  computed,
  ViewChild,
  ElementRef,
  NgZone,
  ChangeDetectorRef,
  DestroyRef,
  ChangeDetectionStrategy
} from '@angular/core';
import { CommonModule } from '@angular/common';
import { ActivatedRoute, Router } from '@angular/router';
import { Subscription } from 'rxjs';
import { auditTime, filter } from 'rxjs/operators';
import { EvaluationsService } from '@core/services/evaluations.service';
import { LiveService } from '@core/services/live.service';
import { BreadcrumbsService } from '@core/services/breadcrumbs.service';
import { type BuildOrigin, links, readBuildOrigin } from '@shared/routing';
import {
  BadgeComponent,
  BadgeSeverity,
  BreadcrumbComponent,
  IconComponent,
  LoadStateComponent,
  LoadingSpinnerComponent,
} from '@gradient/ui/ui';
import { plural } from '@shared/text';
import { PHASE_COLOR, PHASE_SEVERITY, buildDurationText, buildPhase, buildStatusLabel, type StatusPhase } from '@shared/evaluation';
import { longestPathDepths } from './dependency-depths';
import { observeElementResizeUntilDestroyed } from '../observe-element-resize-until-destroyed';
import { BuildGraph, BuildStatus, DependencyEdge, DependencyGraphNode } from '@core/models';

const CARD_W = 200;
const paint = (el: SVGElement, property: 'fill' | 'stroke', role: string) => el.style.setProperty(property, role.startsWith('var(') ? role : `var(${role})`);
const CARD_H = 66;
const H_GAP = 32;
const V_GAP = 90;
const minOpeningZoomScale = 0.6;
const ACTIVE_STATUSES = ['Building', 'Queued', 'Created'];

interface LayoutNode extends DependencyGraphNode {
  x: number;
  y: number;
}

@Component({
  selector: 'app-dependency-graph',
  standalone: true,
  imports: [CommonModule, LoadingSpinnerComponent, LoadStateComponent, IconComponent, BadgeComponent, BreadcrumbComponent],
  templateUrl: './dependency-graph.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './dependency-graph.component.scss',
})
export class DependencyGraphComponent implements OnInit, OnDestroy {
  private route = inject(ActivatedRoute);
  private router = inject(Router);
  private evalService = inject(EvaluationsService);
  private live = inject(LiveService);
  private crumbs = inject(BreadcrumbsService);
  private zone = inject(NgZone);
  private cdr = inject(ChangeDetectorRef);
  private destroyRef = inject(DestroyRef);

  @ViewChild('svgEl') svgRef!: ElementRef<SVGSVGElement>;
  @ViewChild('graphEl') graphRef!: ElementRef<SVGGElement>;
  @ViewChild('legendEl') colourLegendBoxRef?: ElementRef<HTMLElement>;

  loading = signal(true);
  loadError = signal<unknown>(null);

  projectName = '';
  buildId = '';
  private from = signal<BuildOrigin>({});
  breadcrumb = computed(() => this.crumbs.buildPage(this.projectName, this.from(), this.buildId, 'Dependency graph'));
  rootId = signal('');
  nodeCount = signal(0);
  edgeCount = signal(0);
  rootName = signal('');
  rootStatus = signal('');

  // Viewport
  scale = signal(1);
  tx = signal(0);
  ty = signal(0);
  transform = computed(() => `translate(${this.tx()},${this.ty()}) scale(${this.scale()})`);
  showLabels = computed(() => this.scale() > 0.4);

  // Tooltip
  hoveredNode = signal<LayoutNode | null>(null);
  tooltipX = signal(0);
  tooltipY = signal(0);
  private hoveredId: string | null = null;

  private layoutNodes: LayoutNode[] = [];
  private layoutEdges: DependencyEdge[] = [];
  private nodeMap = new Map<string, LayoutNode>();

  // DOM refs (imperative)
  private nodeEls = new Map<string, SVGGElement>();
  private edgeEls: { el: SVGPathElement; source: string; target: string }[] = [];
  private durationEls = new Map<string, SVGTextElement>();
  private incident = new Map<string, Set<number>>(); // nodeId -> indices into edgeEls
  private neighbours = new Map<string, Set<string>>();

  // Interaction
  private isPanning = false;
  private panStart = { x: 0, y: 0, tx: 0, ty: 0 };
  private didDrag = false;

  private liveSub?: Subscription;
  private timerInterval?: ReturnType<typeof setInterval>;

  ngOnInit(): void {
    document.documentElement.style.overflow = 'hidden';
    document.body.style.overflow = 'hidden';
    this.projectName = this.route.snapshot.paramMap.get('project') || '';
    this.buildId = this.route.snapshot.paramMap.get('buildId') || '';
    this.from.set(readBuildOrigin(this.route.snapshot.queryParamMap));
    this.loadGraph();
  }

  ngOnDestroy(): void {
    document.documentElement.style.overflow = '';
    document.body.style.overflow = '';
    this.liveSub?.unsubscribe();
    if (this.timerInterval) clearInterval(this.timerInterval);
  }

  loadGraph(): void {
    this.loading.set(true);
    this.loadError.set(null);
    this.evalService.getBuildGraph(this.buildId).subscribe({
      next: (graph) => {
        this.rootId.set(graph.root);
        this.nodeCount.set(graph.nodes.length);
        this.edgeCount.set(graph.edges.length);
        const root = graph.nodes.find((n) => n.id === graph.root);
        this.rootName.set(root?.name ?? '');
        this.rootStatus.set(root?.status ?? '');

        if (!this.from().evaluation) {
          this.evalService.getBuild(this.buildId).subscribe({
            next: (build) => this.from.update((from) => ({ ...from, evaluation: build.evaluation })),
            error: () => {},
          });
        }
        this.initLayout(graph);
        this.loading.set(false);
        this.cdr.detectChanges();

        setTimeout(() => {
          this.createSvgElements();
          this.centerViewport();
          if (this.svgRef) observeElementResizeUntilDestroyed(this.svgRef.nativeElement, this.destroyRef, () => this.centerViewport());
          this.startLiveUpdates();
          this.startTimer();
        }, 0);
      },
      error: (error: unknown) => {
        this.loading.set(false);
        this.loadError.set(error);
      },
    });
  }

  // ── Layout ────────────────────────────────────────────────────────────────

  private initLayout(graph: BuildGraph): void {
    this.layoutNodes = graph.nodes.map((n) => ({ ...n, x: 0, y: 0 }));
    this.layoutEdges = graph.edges;
    this.nodeMap = new Map(this.layoutNodes.map((n) => [n.id, n]));
    this.computeTreeLayout();
  }

  private computeTreeLayout(): void {
    const rootId = this.rootId();
    const nodes = this.layoutNodes;
    const edges = this.layoutEdges;

    const wantedBy = new Map<string, string[]>();
    for (const n of nodes) wantedBy.set(n.id, []);
    for (const e of edges) wantedBy.get(e.source)?.push(e.target);

    const depth = longestPathDepths(rootId, edges);

    // Group nodes by level
    const byLevel = new Map<number, LayoutNode[]>();
    for (const n of nodes) {
      const d = depth.get(n.id) ?? 0;
      if (!byLevel.has(d)) byLevel.set(d, []);
      byLevel.get(d)!.push(n);
    }

    // Position level by level, sorting each level by average parent X to reduce crossings
    const posX = new Map<string, number>();
    const maxLevel = Math.max(...depth.values(), 0);

    for (let lv = 0; lv <= maxLevel; lv++) {
      const group = byLevel.get(lv) || [];

      if (lv > 0) {
        const parentX = new Map(group.map((n) => [n.id, this.avgX(n.id, wantedBy, posX)]));
        group.sort((a, b) => parentX.get(a.id)! - parentX.get(b.id)!);
      }

      const totalW = group.length * CARD_W + Math.max(0, group.length - 1) * H_GAP;
      const startX = -totalW / 2;
      group.forEach((n, i) => {
        n.x = startX + i * (CARD_W + H_GAP);
        n.y = lv * (CARD_H + V_GAP);
        posX.set(n.id, n.x + CARD_W / 2);
      });
    }
  }

  private avgX(nodeId: string, wantedBy: Map<string, string[]>, posX: Map<string, number>): number {
    const parentIds = wantedBy.get(nodeId) || [];
    const xs = parentIds.map((pid) => posX.get(pid)).filter((x): x is number => x !== undefined);
    if (!xs.length) return 0;
    return xs.reduce((a, b) => a + b, 0) / xs.length;
  }

  // ── Imperative SVG creation ───────────────────────────────────────────────

  private createSvgElements(): void {
    const g = this.graphRef?.nativeElement;
    if (!g) return;

    g.innerHTML = '';
    this.nodeEls.clear();
    this.durationEls.clear();
    this.edgeEls = [];

    // Edges first (rendered under cards)
    this.layoutEdges.forEach((edge) => {
      const path = document.createElementNS('http://www.w3.org/2000/svg', 'path');
      path.setAttribute('fill', 'none');
      path.setAttribute('stroke-width', '1.5');
      path.setAttribute('marker-end', 'url(#arrowhead)');
      g.appendChild(path);
      this.edgeEls.push({ el: path, source: edge.source, target: edge.target });
    });

    // Cards
    for (const node of this.layoutNodes) {
      const cardG = this.createCard(node);
      g.appendChild(cardG);
      this.nodeEls.set(node.id, cardG);
    }

    // Incidence index: which edges + neighbours touch each node, for hover highlight.
    this.incident.clear();
    this.neighbours.clear();
    this.edgeEls.forEach((e, i) => {
      for (const id of [e.source, e.target]) {
        if (!this.incident.has(id)) this.incident.set(id, new Set());
        this.incident.get(id)!.add(i);
        if (!this.neighbours.has(id)) this.neighbours.set(id, new Set());
      }
      this.neighbours.get(e.source)!.add(e.target);
      this.neighbours.get(e.target)!.add(e.source);
    });

    this.updateSvgPositions();
  }

  private createCard(node: LayoutNode): SVGGElement {
    const isRoot = node.id === this.rootId();
    const color = this.nodeColor(node.status);
    const ns = 'http://www.w3.org/2000/svg';

    const g = document.createElementNS(ns, 'g');
    g.setAttribute('cursor', node.build ? 'pointer' : 'default');
    g.setAttribute('data-id', node.id);

    // Drop shadow
    const shadow = document.createElementNS(ns, 'rect');
    shadow.setAttribute('x', '2'); shadow.setAttribute('y', '3');
    shadow.setAttribute('width', String(CARD_W)); shadow.setAttribute('height', String(CARD_H));
    shadow.setAttribute('rx', '8'); paint(shadow, 'fill', '--gr-border-subtle'); shadow.setAttribute('opacity', '0.5');
    g.appendChild(shadow);

    // Card background
    const bg = document.createElementNS(ns, 'rect');
    bg.setAttribute('width', String(CARD_W)); bg.setAttribute('height', String(CARD_H));
    bg.setAttribute('rx', '8');
    paint(bg, 'fill', isRoot ? '--gr-surface-active' : '--gr-surface-raised');
    paint(bg, 'stroke', isRoot ? color : '--gr-border');
    bg.setAttribute('stroke-width', isRoot ? '1.5' : '1');
    g.appendChild(bg);

    // Status stripe (4 px left edge)
    const stripe = document.createElementNS(ns, 'rect');
    stripe.setAttribute('width', '4'); stripe.setAttribute('height', String(CARD_H - 16));
    stripe.setAttribute('x', '0'); stripe.setAttribute('y', '8');
    stripe.setAttribute('rx', '2'); paint(stripe, 'fill', color);
    g.appendChild(stripe);

    // Package name  [text index 0]
    const nameText = document.createElementNS(ns, 'text');
    nameText.setAttribute('x', '14'); nameText.setAttribute('y', '22');
    paint(nameText, 'fill', '--gr-text-primary');
    nameText.setAttribute('font-size', '13');
    nameText.setAttribute('font-weight', '600');
    nameText.setAttribute('font-family', 'monospace, Arial, sans-serif');
    nameText.setAttribute('pointer-events', 'none');
    nameText.textContent = node.name.length > 22 ? node.name.slice(0, 20) + '…' : node.name;
    g.appendChild(nameText);

    // Status label  [text index 1]
    const statusText = document.createElementNS(ns, 'text');
    statusText.setAttribute('x', '14'); statusText.setAttribute('y', '40');
    paint(statusText, 'fill', '--gr-text-secondary');
    statusText.setAttribute('font-size', '11');
    statusText.setAttribute('font-family', 'Arial, sans-serif');
    statusText.setAttribute('pointer-events', 'none');
    statusText.textContent = this.statusLabel(node.status);
    g.appendChild(statusText);

    // Build duration  [text index 2]
    const durationText = document.createElementNS(ns, 'text') as SVGTextElement;
    durationText.setAttribute('x', '14'); durationText.setAttribute('y', '54');
    paint(durationText, 'fill', '--gr-text-muted');
    durationText.setAttribute('font-size', '10');
    durationText.setAttribute('font-family', 'Arial, sans-serif');
    durationText.setAttribute('pointer-events', 'none');
    durationText.textContent = this.calcDuration(node);
    g.appendChild(durationText);
    this.durationEls.set(node.id, durationText);

    // Hover events for tooltip
    g.addEventListener('mouseenter', (e: MouseEvent) => {
      this.highlightNode(node.id);
      this.zone.run(() => {
        this.hoveredNode.set(node);
        this.tooltipX.set(e.clientX + 14);
        this.tooltipY.set(e.clientY - 10);
      });
    });
    g.addEventListener('mouseleave', () => {
      this.resetHighlight();
      this.zone.run(() => this.hoveredNode.set(null));
    });
    if (node.build) {
      g.setAttribute('tabindex', '0');
      g.setAttribute('role', 'link');
      g.addEventListener('click', () => {
        if (!this.didDrag) this.navigateToEvaluationLogForNode(node);
      });
      g.addEventListener('keydown', (e: KeyboardEvent) => {
        if (e.key !== 'Enter' && e.key !== ' ') return;
        e.preventDefault();
        this.navigateToEvaluationLogForNode(node);
      });
      g.addEventListener('focus', () => this.highlightNode(node.id));
      g.addEventListener('blur', () => this.resetHighlight());
    }

    return g;
  }

  private navigateToEvaluationLogForNode(node: LayoutNode): void {
    const evaluation = this.from().evaluation;
    if (!evaluation || !node.build) return;
    this.zone.run(() => this.router.navigateByUrl(links.evaluationLog(this.projectName, evaluation, { build: node.build })));
  }

  private updateSvgPositions(): void {
    // Update card transforms and live status visuals
    for (const node of this.layoutNodes) {
      const el = this.nodeEls.get(node.id);
      if (!el) continue;
      el.setAttribute('transform', `translate(${node.x.toFixed(1)},${node.y.toFixed(1)})`);

      const color = this.nodeColor(node.status);
      const stripe = el.querySelectorAll('rect')[2];
      if (stripe) paint(stripe, 'fill', color);

      if (node.id === this.rootId()) {
        const bg = el.querySelectorAll('rect')[1];
        if (bg) paint(bg, 'stroke', color);
      }
    }

    // Update edge paths - every edge uses a smooth bottom-to-top bezier; hovering a
    // node (highlightNode) makes its incident edges legible while hovered.
    for (const edge of this.edgeEls) {
      const child = this.nodeMap.get(edge.source);
      const parent = this.nodeMap.get(edge.target);
      if (!child || !parent) continue;
      const px = parent.x + CARD_W / 2, py = parent.y + CARD_H;
      const cx = child.x + CARD_W / 2, cy = child.y;
      const midY = (py + cy) / 2;
      edge.el.setAttribute('d', `M ${px} ${py} C ${px} ${midY} ${cx} ${midY} ${cx} ${cy}`);
      paint(edge.el, 'stroke', '--gr-border');
    }

    // Re-apply hover highlight if a node is still hovered after a live update.
    if (this.hoveredId) this.highlightNode(this.hoveredId);
  }

  private highlightNode(nodeId: string): void {
    this.hoveredId = nodeId;
    const inc = this.incident.get(nodeId) ?? new Set<number>();
    const near = this.neighbours.get(nodeId) ?? new Set<string>();
    this.edgeEls.forEach((e, i) => {
      const on = inc.has(i);
      paint(e.el, 'stroke', on ? '--gr-text-secondary' : '--gr-border-subtle');
      e.el.setAttribute('stroke-width', on ? '2.5' : '1');
      e.el.setAttribute('opacity', on ? '1' : '0.25');
    });
    for (const [id, el] of this.nodeEls) {
      el.setAttribute('opacity', id === nodeId || near.has(id) ? '1' : '0.35');
    }
  }

  private resetHighlight(): void {
    this.hoveredId = null;
    for (const e of this.edgeEls) {
      paint(e.el, 'stroke', '--gr-border');
      e.el.setAttribute('stroke-width', '1.5');
      e.el.setAttribute('opacity', '1');
    }
    for (const [, el] of this.nodeEls) el.setAttribute('opacity', '1');
  }

  private centerViewport(): void {
    const svg = this.svgRef?.nativeElement;
    if (!svg || !this.layoutNodes.length) return;
    const rect = svg.getBoundingClientRect();

    let minX = Infinity, maxX = -Infinity, minY = Infinity, maxY = -Infinity;
    for (const n of this.layoutNodes) {
      if (n.x < minX) minX = n.x; if (n.x + CARD_W > maxX) maxX = n.x + CARD_W;
      if (n.y < minY) minY = n.y; if (n.y + CARD_H > maxY) maxY = n.y + CARD_H;
    }

    const legendTop = this.colourLegendBoxRef?.nativeElement.getBoundingClientRect().top ?? rect.bottom;
    const visibleHeight = Math.max(rect.height / 2, Math.min(rect.height, legendTop - rect.top));
    const pad = 60;
    const gw = maxX - minX + pad * 2;
    const gh = maxY - minY + pad * 2;
    const s = Math.min(rect.width / gw, visibleHeight / gh, 1.6);
    const root = this.layoutNodes.find((n) => n.id === this.rootId());
    if (s < minOpeningZoomScale && root) {
      this.scale.set(minOpeningZoomScale);
      this.tx.set(rect.width / 2 - (root.x + CARD_W / 2) * minOpeningZoomScale);
      this.ty.set(visibleHeight / 2 - (root.y + CARD_H / 2) * minOpeningZoomScale);
      return;
    }
    this.scale.set(s);
    this.tx.set(rect.width / 2 - ((minX + maxX) / 2) * s);
    this.ty.set(visibleHeight / 2 - ((minY + maxY) / 2) * s);
  }

  // ── Interaction ───────────────────────────────────────────────────────────

  onWheel(event: WheelEvent): void {
    event.preventDefault();
    const rect = this.svgRef.nativeElement.getBoundingClientRect();
    const mx = event.clientX - rect.left;
    const my = event.clientY - rect.top;
    const oldScale = this.scale();
    const target = oldScale * (event.deltaY > 0 ? 0.9 : 1.1);
    const newScale = Math.max(0.08, Math.min(4, target));
    const effFactor = newScale / oldScale;
    if (effFactor === 1) return;
    this.scale.set(newScale);
    this.tx.update((v) => mx - effFactor * (mx - v));
    this.ty.update((v) => my - effFactor * (my - v));
  }

  onSvgMousedown(event: MouseEvent): void {
    if (event.button !== 0) return;
    this.isPanning = true;
    this.didDrag = false;
    this.panStart = { x: event.clientX, y: event.clientY, tx: this.tx(), ty: this.ty() };
  }

  onMousemove(event: MouseEvent): void {
    if (this.isPanning) {
      const dx = event.clientX - this.panStart.x;
      const dy = event.clientY - this.panStart.y;
      if (!this.didDrag && (Math.abs(dx) > 4 || Math.abs(dy) > 4)) {
        this.didDrag = true;
      }
      this.tx.set(this.panStart.tx + dx);
      this.ty.set(this.panStart.ty + dy);
    }
    if (this.hoveredNode()) {
      this.tooltipX.set(event.clientX + 14);
      this.tooltipY.set(event.clientY - 10);
    }
  }

  onMouseup(): void {
    this.isPanning = false;
  }

  private zoomBy(factor: number): void {
    const rect = this.svgRef?.nativeElement.getBoundingClientRect();
    if (!rect) return;
    const cx = rect.width / 2, cy = rect.height / 2;
    const oldScale = this.scale();
    const newScale = Math.max(0.08, Math.min(4, oldScale * factor));
    const eff = newScale / oldScale;
    if (eff === 1) return;
    this.scale.set(newScale);
    this.tx.update((v) => cx - eff * (cx - v));
    this.ty.update((v) => cy - eff * (cy - v));
  }

  zoomIn(): void { this.zoomBy(1.3); }
  zoomOut(): void { this.zoomBy(1 / 1.3); }

  fitToScreen(): void {
    this.centerViewport();
  }

  // ── Live updates ──────────────────────────────────────────────────────────

  private startLiveUpdates(): void {
    this.liveSub = this.live
      .connect(`/builds/${this.buildId}/live`)
      .pipe(filter((e) => e.event !== 'build.progress'), auditTime(300))
      .subscribe(() => {
        const hasActive = this.layoutNodes.some((n) => this.isActive(n.status));
        if (!hasActive) { this.liveSub?.unsubscribe(); return; }

        this.evalService.getBuildGraph(this.buildId).subscribe({
          next: (graph) => {
            let changed = false;
            for (const updated of graph.nodes) {
              const existing = this.nodeMap.get(updated.id);
              if (existing && existing.status !== updated.status) {
                existing.status = updated.status;
                existing.build_time_ms = updated.build_time_ms;
                existing.build_started_at = updated.build_started_at;
                changed = true;
              }
            }
            if (changed) {
              const root = graph.nodes.find((n) => n.id === graph.root);
              this.zone.run(() => this.rootStatus.set(root?.status ?? ''));
              this.updateSvgPositions();
            }
          },
        });
      });
  }

  // ── Live timer ────────────────────────────────────────────────────────────

  private startTimer(): void {
    this.zone.runOutsideAngular(() => {
      this.timerInterval = setInterval(() => {
        for (const [nodeId, el] of this.durationEls) {
          const node = this.nodeMap.get(nodeId);
          if (!node) continue;
          if (this.isActive(node.status)) {
            el.textContent = this.calcDuration(node);
          }
        }
      }, 1000);
    });
  }

  // ── Helpers ───────────────────────────────────────────────────────────────

  private isActive(status?: string | null): boolean {
    return status != null && ACTIVE_STATUSES.includes(status);
  }

  protected readonly plural = plural;

  statusLabel(status?: string | null): string {
    return status ? buildStatusLabel(status as BuildStatus) : 'No build';
  }

  private calcDuration(node: LayoutNode): string {
    return node.status == null ? '' : buildDurationText({ ...node, status: node.status }, Date.now());
  }

  private phase(status?: string | null): StatusPhase {
    return (status && buildPhase(status as BuildStatus)) || 'aborted';
  }

  nodeColor(status?: string | null): string {
    return PHASE_COLOR[this.phase(status)];
  }

  /// The legend shows one entry per node colour and lists every status drawn in that colour, so two entries never share a colour.
  readonly legend: { label: string; color: string }[] = (() => {
    const labelsByColor = new Map<string, string[]>();
    for (const status of ['Created', 'Queued', 'Building', 'Completed', 'Substituted', 'FailedPermanent', 'FailedTransient', 'FailedTimeout', 'Aborted', 'DependencyFailed', 'Skipped'] as const) {
      const color = PHASE_COLOR[buildPhase(status)];
      labelsByColor.set(color, [...(labelsByColor.get(color) ?? []), buildStatusLabel(status)]);
    }
    return [...labelsByColor].map(([color, labels]) => ({ color, label: labels.join(', ') }));
  })();

  statusSeverity(status?: string | null): BadgeSeverity {
    return PHASE_SEVERITY[this.phase(status)];
  }
}
