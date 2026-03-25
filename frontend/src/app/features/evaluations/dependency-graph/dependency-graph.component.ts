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
} from '@angular/core';
import { CommonModule } from '@angular/common';
import { ActivatedRoute, Router, RouterModule } from '@angular/router';
import { interval, Subscription } from 'rxjs';
import { EvaluationsService, BuildGraph } from '@core/services/evaluations.service';
import { LoadingSpinnerComponent } from '@shared/components/loading-spinner/loading-spinner.component';
import { ButtonModule } from 'primeng/button';

const CARD_W = 200;
const CARD_H = 78;
const H_GAP = 32;
const V_GAP = 90;

interface LayoutNode {
  id: string;
  name: string;
  path: string;
  status: string;
  created_at: string;
  updated_at: string;
  x: number;
  y: number;
}

interface LayoutEdge {
  source: string; // dependency (child in tree — lower level)
  target: string; // dependent  (parent in tree — higher level)
}

@Component({
  selector: 'app-dependency-graph',
  standalone: true,
  imports: [CommonModule, RouterModule, LoadingSpinnerComponent, ButtonModule],
  templateUrl: './dependency-graph.component.html',
  styleUrl: './dependency-graph.component.scss',
})
export class DependencyGraphComponent implements OnInit, OnDestroy {
  private route = inject(ActivatedRoute);
  private router = inject(Router);
  private evalService = inject(EvaluationsService);
  private zone = inject(NgZone);
  private cdr = inject(ChangeDetectorRef);

  @ViewChild('svgEl') svgRef!: ElementRef<SVGSVGElement>;
  @ViewChild('graphEl') graphRef!: ElementRef<SVGGElement>;

  loading = signal(true);
  errorMsg = signal<string | null>(null);

  orgName = '';
  buildId = '';
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

  private layoutNodes: LayoutNode[] = [];
  private layoutEdges: LayoutEdge[] = [];
  private nodeMap = new Map<string, LayoutNode>();
  private nodeDepths = new Map<string, number>();
  private graphBounds = { minX: 0, maxX: 0 };

  // DOM refs (imperative)
  private nodeEls = new Map<string, SVGGElement>();
  private edgeEls: { el: SVGPathElement; source: string; target: string; idx: number }[] = [];
  private durationEls = new Map<string, SVGTextElement>();

  // Interaction
  private isPanning = false;
  private panStart = { x: 0, y: 0, tx: 0, ty: 0 };

  private pollSub?: Subscription;
  private timerInterval?: ReturnType<typeof setInterval>;

  ngOnInit(): void {
    this.orgName = this.route.snapshot.paramMap.get('org') || '';
    this.buildId = this.route.snapshot.paramMap.get('buildId') || '';
    this.loadGraph();
  }

  ngOnDestroy(): void {
    this.pollSub?.unsubscribe();
    if (this.timerInterval) clearInterval(this.timerInterval);
  }

  loadGraph(): void {
    this.loading.set(true);
    this.errorMsg.set(null);
    this.evalService.getBuildGraph(this.buildId).subscribe({
      next: (graph) => {
        this.rootId.set(graph.root);
        this.nodeCount.set(graph.nodes.length);
        this.edgeCount.set(graph.edges.length);
        const root = graph.nodes.find((n) => n.id === graph.root);
        this.rootName.set(root?.name ?? '');
        this.rootStatus.set(root?.status ?? '');

        this.initLayout(graph);
        this.loading.set(false);
        this.cdr.detectChanges();

        setTimeout(() => {
          this.createSvgElements();
          this.centerViewport();
          this.startPolling();
          this.startTimer();
        }, 0);
      },
      error: () => {
        this.loading.set(false);
        this.errorMsg.set('Failed to load dependency graph');
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

    // deps.get(N)       = what N directly depends on (tree children — built first)
    // dependents.get(N) = what directly depends on N (tree parents — built after)
    const deps = new Map<string, string[]>();
    const dependents = new Map<string, string[]>();
    for (const n of nodes) { deps.set(n.id, []); dependents.set(n.id, []); }
    for (const e of edges) {
      deps.get(e.target)?.push(e.source);
      dependents.get(e.source)?.push(e.target);
    }

    // BFS assigning MAXIMUM depth (longest path from root → minimises upward long-edges)
    const depth = new Map<string, number>();
    depth.set(rootId, 0);
    const q: string[] = [rootId];
    let qi = 0;
    while (qi < q.length) {
      const id = q[qi++];
      const d = depth.get(id)!;
      for (const child of (deps.get(id) || [])) {
        if ((depth.get(child) ?? -1) < d + 1) {
          depth.set(child, d + 1);
          q.push(child);
        }
      }
    }

    this.nodeDepths = depth;

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
        group.sort((a, b) => {
          const ax = this.avgX(a.id, dependents, posX);
          const bx = this.avgX(b.id, dependents, posX);
          return ax - bx;
        });
      }

      const totalW = group.length * CARD_W + Math.max(0, group.length - 1) * H_GAP;
      const startX = -totalW / 2;
      group.forEach((n, i) => {
        n.x = startX + i * (CARD_W + H_GAP);
        n.y = lv * (CARD_H + V_GAP);
        posX.set(n.id, n.x + CARD_W / 2);
      });
    }

    // Overall graph bounds (used for side-lane routing of multi-level edges)
    this.graphBounds = { minX: Infinity, maxX: -Infinity };
    for (const n of nodes) {
      if (n.x < this.graphBounds.minX) this.graphBounds.minX = n.x;
      if (n.x + CARD_W > this.graphBounds.maxX) this.graphBounds.maxX = n.x + CARD_W;
    }
  }

  private avgX(nodeId: string, dependents: Map<string, string[]>, posX: Map<string, number>): number {
    const parentIds = dependents.get(nodeId) || [];
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
    this.layoutEdges.forEach((edge, idx) => {
      const path = document.createElementNS('http://www.w3.org/2000/svg', 'path');
      path.setAttribute('fill', 'none');
      path.setAttribute('stroke-width', '1.5');
      path.setAttribute('marker-end', 'url(#arrowhead)');
      g.appendChild(path);
      this.edgeEls.push({ el: path, source: edge.source, target: edge.target, idx });
    });

    // Cards
    for (const node of this.layoutNodes) {
      const cardG = this.createCard(node);
      g.appendChild(cardG);
      this.nodeEls.set(node.id, cardG);
    }

    this.updateSvgPositions();
  }

  private createCard(node: LayoutNode): SVGGElement {
    const isRoot = node.id === this.rootId();
    const color = this.nodeColor(node.status);
    const ns = 'http://www.w3.org/2000/svg';

    const g = document.createElementNS(ns, 'g');
    g.setAttribute('cursor', 'pointer');
    g.setAttribute('data-id', node.id);

    // Drop shadow
    const shadow = document.createElementNS(ns, 'rect');
    shadow.setAttribute('x', '2'); shadow.setAttribute('y', '3');
    shadow.setAttribute('width', String(CARD_W)); shadow.setAttribute('height', String(CARD_H));
    shadow.setAttribute('rx', '8'); shadow.setAttribute('fill', '#0d1118'); shadow.setAttribute('opacity', '0.5');
    g.appendChild(shadow);

    // Card background
    const bg = document.createElementNS(ns, 'rect');
    bg.setAttribute('width', String(CARD_W)); bg.setAttribute('height', String(CARD_H));
    bg.setAttribute('rx', '8');
    bg.setAttribute('fill', isRoot ? '#1a2535' : '#1c2333');
    bg.setAttribute('stroke', isRoot ? color : '#2d3748');
    bg.setAttribute('stroke-width', isRoot ? '1.5' : '1');
    g.appendChild(bg);

    // Status stripe (4 px left edge)
    const stripe = document.createElementNS(ns, 'rect');
    stripe.setAttribute('width', '4'); stripe.setAttribute('height', String(CARD_H - 16));
    stripe.setAttribute('x', '0'); stripe.setAttribute('y', '8');
    stripe.setAttribute('rx', '2'); stripe.setAttribute('fill', color);
    g.appendChild(stripe);

    // Package name  [text index 0]
    const nameText = document.createElementNS(ns, 'text');
    nameText.setAttribute('x', '14'); nameText.setAttribute('y', '22');
    nameText.setAttribute('fill', '#e5e7eb');
    nameText.setAttribute('font-size', '13');
    nameText.setAttribute('font-weight', '600');
    nameText.setAttribute('font-family', 'monospace, Arial, sans-serif');
    nameText.setAttribute('pointer-events', 'none');
    nameText.textContent = node.name.length > 22 ? node.name.slice(0, 20) + '…' : node.name;
    g.appendChild(nameText);

    // Derivation filename (strip /nix/store/hash- prefix)  [text index 1]
    const filename = node.path.split('/').pop() ?? '';
    const dashIdx = filename.indexOf('-');
    const drvName = dashIdx >= 0 ? filename.slice(dashIdx + 1) : filename;
    const pathText = document.createElementNS(ns, 'text');
    pathText.setAttribute('x', '14'); pathText.setAttribute('y', '36');
    pathText.setAttribute('fill', '#4b5563');
    pathText.setAttribute('font-size', '10');
    pathText.setAttribute('font-family', 'monospace, Arial, sans-serif');
    pathText.setAttribute('pointer-events', 'none');
    pathText.textContent = drvName.length > 26 ? drvName.slice(0, 25) + '…' : drvName;
    g.appendChild(pathText);

    // Status label  [text index 2]
    const statusText = document.createElementNS(ns, 'text');
    statusText.setAttribute('x', '14'); statusText.setAttribute('y', '52');
    statusText.setAttribute('fill', color);
    statusText.setAttribute('font-size', '11');
    statusText.setAttribute('font-family', 'Arial, sans-serif');
    statusText.setAttribute('pointer-events', 'none');
    statusText.textContent = node.status;
    g.appendChild(statusText);

    // Build duration  [text index 3]
    const durationText = document.createElementNS(ns, 'text') as SVGTextElement;
    durationText.setAttribute('x', '14'); durationText.setAttribute('y', '66');
    durationText.setAttribute('fill', '#4b5563');
    durationText.setAttribute('font-size', '10');
    durationText.setAttribute('font-family', 'Arial, sans-serif');
    durationText.setAttribute('pointer-events', 'none');
    durationText.textContent = this.calcDuration(node);
    g.appendChild(durationText);
    this.durationEls.set(node.id, durationText);

    // Hover events for tooltip
    g.addEventListener('mouseenter', (e: MouseEvent) => {
      this.zone.run(() => {
        this.hoveredNode.set(node);
        this.tooltipX.set(e.clientX + 14);
        this.tooltipY.set(e.clientY - 10);
      });
    });
    g.addEventListener('mouseleave', () => {
      this.zone.run(() => this.hoveredNode.set(null));
    });
    // Click navigates to build log
    g.addEventListener('click', () => {
      const evalId = this.route.snapshot.queryParamMap.get('evalId');
      if (evalId) {
        this.zone.run(() =>
          this.router.navigate(['/organization', this.orgName, 'log', evalId], {
            queryParams: { build: node.id },
          })
        );
      }
    });

    return g;
  }

  private updateSvgPositions(): void {
    const LANE_MARGIN = 50;

    // Update card transforms and live status visuals
    for (const node of this.layoutNodes) {
      const el = this.nodeEls.get(node.id);
      if (!el) continue;
      el.setAttribute('transform', `translate(${node.x.toFixed(1)},${node.y.toFixed(1)})`);

      const color = this.nodeColor(node.status);
      // rect[2] = stripe, text[2] = status label
      const stripe = el.querySelectorAll('rect')[2];
      if (stripe) stripe.setAttribute('fill', color);
      const statusTxt = el.querySelectorAll('text')[2];
      if (statusTxt) statusTxt.setAttribute('fill', color);

      if (node.id === this.rootId()) {
        const bg = el.querySelectorAll('rect')[1];
        if (bg) bg.setAttribute('stroke', color);
      }
    }

    // Update edge paths
    for (const edge of this.edgeEls) {
      const child  = this.nodeMap.get(edge.source); // dependency  (lower level)
      const parent = this.nodeMap.get(edge.target); // dependent   (higher level)
      if (!child || !parent) continue;

      const pd = this.nodeDepths.get(edge.target) ?? 0;
      const cd = this.nodeDepths.get(edge.source) ?? 0;
      const dd = cd - pd;

      const pCX = parent.x + CARD_W / 2;
      const cCX = child.x + CARD_W / 2;

      if (dd <= 1) {
        // Single-level: smooth bezier bottom-center → top-center
        const px = pCX, py = parent.y + CARD_H;
        const cx = cCX, cy = child.y;
        const midY = (py + cy) / 2;
        edge.el.setAttribute('d', `M ${px} ${py} C ${px} ${midY} ${cx} ${midY} ${cx} ${cy}`);
        edge.el.setAttribute('stroke', '#374151');
      } else {
        // Multi-level: route through a side-lane to avoid crossing intermediate cards.
        // Determine which side based on horizontal direction; use edge index as tiebreaker.
        const dx = cCX - pCX;
        const useRight = dx > 5 ? true : dx < -5 ? false : edge.idx % 2 === 0;
        const laneX = useRight
          ? this.graphBounds.maxX + LANE_MARGIN + (dd - 2) * 15
          : this.graphBounds.minX - LANE_MARGIN - (dd - 2) * 15;

        // Exit from the card's left/right side mid-height, re-enter child top-center vertically.
        const exitX = useRight ? parent.x + CARD_W : parent.x;
        const exitY = parent.y + CARD_H / 2;
        const TURN = 22;

        // Compound path: curve to lane → straight down → curve back to child top-center
        const d = [
          `M ${exitX} ${exitY}`,
          `C ${laneX} ${exitY} ${laneX} ${exitY + TURN} ${laneX} ${exitY + TURN}`,
          `L ${laneX} ${child.y - TURN}`,
          `C ${laneX} ${child.y} ${cCX} ${child.y - TURN} ${cCX} ${child.y}`,
        ].join(' ');
        edge.el.setAttribute('d', d);
        edge.el.setAttribute('stroke', '#2d3748');
      }
    }
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

    const pad = 60;
    const gw = maxX - minX + pad * 2;
    const gh = maxY - minY + pad * 2;
    const s = Math.min(rect.width / gw, rect.height / gh, 1.2);
    this.scale.set(s);
    this.tx.set(rect.width / 2 - ((minX + maxX) / 2) * s);
    this.ty.set(rect.height / 2 - ((minY + maxY) / 2) * s);
  }

  // ── Interaction ───────────────────────────────────────────────────────────

  onWheel(event: WheelEvent): void {
    event.preventDefault();
    const factor = event.deltaY > 0 ? 0.9 : 1.1;
    const newScale = Math.max(0.08, Math.min(4, this.scale() * factor));
    const rect = this.svgRef.nativeElement.getBoundingClientRect();
    const mx = event.clientX - rect.left;
    const my = event.clientY - rect.top;
    const dx = (mx - this.tx()) * (1 - factor);
    const dy = (my - this.ty()) * (1 - factor);
    this.scale.set(newScale);
    this.tx.update((v) => v + dx);
    this.ty.update((v) => v + dy);
  }

  onSvgMousedown(event: MouseEvent): void {
    if (event.button !== 0) return;
    this.isPanning = true;
    this.panStart = { x: event.clientX, y: event.clientY, tx: this.tx(), ty: this.ty() };
  }

  onMousemove(event: MouseEvent): void {
    if (this.isPanning) {
      this.tx.set(this.panStart.tx + event.clientX - this.panStart.x);
      this.ty.set(this.panStart.ty + event.clientY - this.panStart.y);
    }
    if (this.hoveredNode()) {
      this.tooltipX.set(event.clientX + 14);
      this.tooltipY.set(event.clientY - 10);
    }
  }

  onMouseup(): void {
    this.isPanning = false;
  }

  zoomIn(): void {
    const rect = this.svgRef?.nativeElement.getBoundingClientRect();
    if (!rect) return;
    const factor = 1.3;
    const cx = rect.width / 2, cy = rect.height / 2;
    this.scale.update((s) => Math.min(s * factor, 4));
    this.tx.update((v) => v + (cx - v) * (1 - 1 / factor));
    this.ty.update((v) => v + (cy - v) * (1 - 1 / factor));
  }

  zoomOut(): void {
    const rect = this.svgRef?.nativeElement.getBoundingClientRect();
    if (!rect) return;
    const factor = 1 / 1.3;
    const cx = rect.width / 2, cy = rect.height / 2;
    this.scale.update((s) => Math.max(s * factor, 0.08));
    this.tx.update((v) => v + (cx - v) * (1 - 1 / factor));
    this.ty.update((v) => v + (cy - v) * (1 - 1 / factor));
  }

  fitToScreen(): void {
    this.centerViewport();
  }

  // ── Live polling ──────────────────────────────────────────────────────────

  private startPolling(): void {
    this.pollSub = interval(5000).subscribe(() => {
      const hasActive = this.layoutNodes.some(
        (n) => n.status === 'Building' || n.status === 'Queued' || n.status === 'Created'
      );
      if (!hasActive) { this.pollSub?.unsubscribe(); return; }

      this.evalService.getBuildGraph(this.buildId).subscribe({
        next: (graph) => {
          let changed = false;
          for (const updated of graph.nodes) {
            const existing = this.nodeMap.get(updated.id);
            if (existing && existing.status !== updated.status) {
              existing.status = updated.status;
              existing.updated_at = updated.updated_at;
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
          if (['Building', 'Queued', 'Created'].includes(node.status)) {
            el.textContent = this.calcDuration(node);
          }
        }
      }, 1000);
    });
  }

  // ── Helpers ───────────────────────────────────────────────────────────────

  private calcDuration(node: LayoutNode): string {
    const isActive = ['Building', 'Queued', 'Created'].includes(node.status);
    const toUtc = (s: string) => new Date(s.includes('Z') || s.includes('+') ? s : s + 'Z').getTime();
    const startMs = toUtc(node.created_at);
    const endMs = isActive ? Date.now() : toUtc(node.updated_at);
    const s = Math.floor((endMs - startMs) / 1000);
    if (isNaN(s) || s < 0) return '';
    if (s < 60) return `${s}s`;
    const m = Math.floor(s / 60);
    return `${m}m ${s % 60}s`;
  }

  nodeColor(status: string): string {
    switch (status) {
      case 'Completed': return '#22c55e';
      case 'Failed':    return '#ef4444';
      case 'Building':  return '#3b82f6';
      case 'Queued':    return '#eab308';
      case 'Aborted':   return '#f97316';
      case 'Created':   return '#6b7280';
      default:          return '#abb0b4';
    }
  }

  statusClass(status: string): string {
    switch (status) {
      case 'Completed':               return 'status-success';
      case 'Failed':                  return 'status-danger';
      case 'Aborted':                 return 'status-warning';
      case 'Building': case 'Queued': return 'status-running';
      default:                        return '';
    }
  }

  goBack(): void {
    const evalId = this.route.snapshot.queryParamMap.get('evalId');
    if (evalId) this.router.navigate(['/organization', this.orgName, 'log', evalId]);
    else this.router.navigate(['/organization', this.orgName]);
  }
}
