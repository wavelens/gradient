/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, OnDestroy, computed, inject, signal } from '@angular/core';
import { CommonModule } from '@angular/common';
import { ActivatedRoute, RouterModule } from '@angular/router';
import { interval, Subscription } from 'rxjs';
import { auditTime } from 'rxjs/operators';
import { ButtonModule } from 'primeng/button';
import { LiveService } from '@core/services/live.service';
import { AuthService } from '@core/services/auth.service';
import { OrganizationsService } from '@core/services/organizations.service';
import { ProjectsService } from '@core/services/projects.service';
import { LoadingSpinnerComponent } from '@shared/components/loading-spinner/loading-spinner.component';
import { EmptyStateComponent } from '@shared/components/empty-state/empty-state.component';
import { AccessService, WritableDirective } from '@shared/access';
import { injectProjectAccess } from '@core/resolvers/inject-access';
import { ProjectDetail, EvaluationSummary, EvaluationStatus, EntryPointSummary, BuildStatus } from '@core/models';
import { formatEvaluationDuration, isRunningEvaluationStatus, parseUtcTimestamp } from '@shared/evaluation';
import { SegmentedBarComponent } from './segmented-bar/segmented-bar.component';

@Component({
  selector: 'app-project-detail',
  standalone: true,
  imports: [
    CommonModule, RouterModule, ButtonModule,
    LoadingSpinnerComponent, EmptyStateComponent, WritableDirective,
    SegmentedBarComponent,
  ],
  templateUrl: './project-detail.component.html',
  styleUrls: ['./project-detail.component.scss', './project-detail.evaluations.scss'],
})
export class ProjectDetailComponent implements OnInit, OnDestroy {
  private route = inject(ActivatedRoute);
  protected authService = inject(AuthService);
  private orgsService = inject(OrganizationsService);
  private projectsService = inject(ProjectsService);
  private accessService = inject(AccessService);
  private live = inject(LiveService);

  access = injectProjectAccess();
  triggerAccess = computed(() => this.accessService.triggerAccess(this.access()));

  loading = signal(true);
  project = signal<ProjectDetail | null>(null);
  entryPoints = signal<EntryPointSummary[]>([]);
  selectedId = signal<string | null>(null);
  starting = signal(false);
  errorMessage = signal<string | null>(null);
  abortTarget = signal<string | null>(null);
  tick = signal(Date.now());

  orgName = '';
  orgDisplayName = signal('');
  projectName = '';

  private liveSub?: Subscription;
  private tickSubscription?: Subscription;

  evaluations = computed(() => this.project()?.last_evaluations ?? []);
  selected = computed<EvaluationSummary | null>(() => {
    const id = this.selectedId();
    const list = this.evaluations();
    return list.find(e => e.id === id) ?? list[0] ?? null;
  });

  ngOnInit(): void {
    this.orgName = this.route.snapshot.paramMap.get('org') || '';
    this.projectName = this.route.snapshot.paramMap.get('project') || '';
    this.orgsService.getOrganization(this.orgName).subscribe({
      next: (org) => this.orgDisplayName.set(org.display_name),
      error: () => {},
    });
    this.loadProjectData();
    this.startLiveUpdates();
    this.tickSubscription = interval(1000).subscribe(() => this.tick.set(Date.now()));
  }

  ngOnDestroy(): void {
    this.liveSub?.unsubscribe();
    this.tickSubscription?.unsubscribe();
  }

  loadProjectData(showLoading = true): void {
    if (showLoading) this.loading.set(true);
    this.projectsService.getProject(this.orgName, this.projectName).subscribe({
      next: (project) => {
        this.project.set(project);
        if (showLoading) this.loading.set(false);
        if (this.starting() && project.last_evaluations.some(e => this.isRunning(e.status))) {
          this.starting.set(false);
        }
        if (!this.selectedId() && project.last_evaluations.length) {
          this.select(project.last_evaluations[0]);
        } else {
          this.loadEntryPoints(this.selected()?.id);
        }
      },
      error: (error) => {
        console.error('Failed to load project:', error);
        if (showLoading) this.loading.set(false);
      },
    });
  }

  select(evaluation: EvaluationSummary): void {
    this.selectedId.set(evaluation.id);
    this.loadEntryPoints(evaluation.id);
  }

  private loadEntryPoints(evaluationId?: string): void {
    if (!evaluationId) { this.entryPoints.set([]); return; }
    this.projectsService.getEntryPoints(this.orgName, this.projectName, evaluationId).subscribe({
      next: (eps) => this.entryPoints.set(
        [...eps].sort((a, b) => this.getDerivationName(a.derivation_path).localeCompare(this.getDerivationName(b.derivation_path))),
      ),
      error: (error) => console.error('Failed to load entry points:', error),
    });
  }

  startEvaluation(): void {
    this.starting.set(true);
    this.errorMessage.set(null);
    this.projectsService.startEvaluation(this.orgName, this.projectName).subscribe({
      next: () => this.loadProjectData(false),
      error: (error) => {
        this.errorMessage.set(error?.message || 'Failed to start evaluation.');
        this.starting.set(false);
      },
    });
  }

  restartFailedBuilds(): void {
    this.starting.set(true);
    this.errorMessage.set(null);
    this.projectsService.restartFailedBuilds(this.orgName, this.projectName).subscribe({
      next: () => this.loadProjectData(false),
      error: (error) => {
        this.errorMessage.set(error?.message || 'Failed to restart failed builds.');
        this.starting.set(false);
      },
    });
  }

  confirmAbort(): void {
    const id = this.abortTarget();
    this.abortTarget.set(null);
    if (!id) return;
    this.projectsService.abortEvaluation(this.orgName, this.projectName, id).subscribe({
      next: () => this.loadProjectData(false),
      error: (error) => console.error('Failed to abort evaluation:', error),
    });
  }

  dismissError(): void { this.errorMessage.set(null); }

  private startLiveUpdates(): void {
    this.liveSub = this.live
      .connect(`/projects/${this.orgName}/${this.projectName}/live`)
      .pipe(auditTime(500))
      .subscribe(() => this.loadProjectData(false));
  }

  isRunning(status: EvaluationStatus): boolean { return isRunningEvaluationStatus(status); }

  evalDuration(evaluation: EvaluationSummary): string {
    const start = parseUtcTimestamp(evaluation.created_at);
    const end = this.isRunning(evaluation.status) ? this.tick() : parseUtcTimestamp(evaluation.updated_at);
    return formatEvaluationDuration(end - start);
  }

  formatDurationMs(ms: number | null): string {
    if (ms == null) return '';
    return formatEvaluationDuration(ms);
  }

  evalTitle(e: EvaluationSummary): string {
    return e.commit_message ?? e.commit.substring(0, 8);
  }

  triggerLabel(e: EvaluationSummary): string {
    if (e.triggered_by) return e.triggered_by;
    switch (e.trigger?.type) {
      case 'polling': return 'Polling';
      case 'reporter_push': return 'Push';
      case 'reporter_pull_request': return 'PR';
      case 'time': return 'Schedule';
      default: return 'Manual';
    }
  }

  getDerivationName(path: string): string {
    const parts = path.split('/').pop() ?? path;
    const match = parts.match(/^[a-z0-9]+-(.+?)(?:\.drv)?$/);
    return match ? match[1] : parts;
  }

  statusClass(status: EvaluationStatus): string {
    switch (status) {
      case 'Completed': return 'ok';
      case 'Failed': return 'err';
      case 'Aborted': return 'muted';
      case 'Waiting': return 'warn';
      default: return 'run';
    }
  }

  statusIcon(status: EvaluationStatus): string {
    switch (status) {
      case 'Completed': return 'check_circle';
      case 'Failed': return 'error';
      case 'Aborted': return 'cancel';
      case 'Waiting': return 'pause_circle';
      case 'Fetching': return 'cloud_download';
      case 'Queued': return 'schedule';
      default: return 'sync';
    }
  }

  buildStatusClass(status: BuildStatus): string {
    switch (status) {
      case 'Completed': case 'Substituted': return 'ok';
      case 'FailedPermanent': case 'FailedTransient': case 'FailedTimeout': return 'err';
      case 'Aborted': case 'DependencyFailed': return 'muted';
      case 'Building': return 'run';
      default: return 'warn';
    }
  }

  buildStatusIcon(status: BuildStatus): string {
    switch (status) {
      case 'Completed': case 'Substituted': return 'check_circle';
      case 'FailedPermanent': case 'FailedTransient': case 'FailedTimeout': return 'error';
      case 'Aborted': case 'DependencyFailed': return 'cancel';
      case 'Building': return 'sync';
      default: return 'schedule';
    }
  }

  depsTotal(ep: EntryPointSummary): number {
    const d = ep.deps;
    return d.completed + d.failed + d.building + d.queued + d.substituted;
  }
}
