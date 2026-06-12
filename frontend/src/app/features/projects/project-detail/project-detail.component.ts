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
import { ProjectDetail, EvaluationSummary, EvaluationStatus, EntryPointSummary, BuildStatus, TriggerType } from '@core/models';
import { formatEvaluationDuration, isRunningEvaluationStatus, parseUtcTimestamp } from '@shared/evaluation';

@Component({
  selector: 'app-project-detail',
  standalone: true,
  imports: [
    CommonModule,
    RouterModule,
    ButtonModule,
    LoadingSpinnerComponent,
    EmptyStateComponent,
    WritableDirective,
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
  /// Access projection for trigger-style actions (Start / Restart / Abort).
  /// Driven by Permission::TriggerEvaluation and not gated by `managed`.
  triggerAccess = computed(() => this.accessService.triggerAccess(this.access()));

  loading = signal(true);
  project = signal<ProjectDetail | null>(null);
  entryPoints = signal<EntryPointSummary[]>([]);
  starting = signal(false);
  errorMessage = signal<string | null>(null);
  tick = signal(Date.now());

  orgName = '';
  orgDisplayName = signal('');
  projectName = '';

  private liveSub?: Subscription;
  private tickSubscription?: Subscription;

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
    this.stopLiveUpdates();
  }

  loadProjectData(showLoading = true): void {
    if (showLoading) this.loading.set(true);
    this.projectsService.getProject(this.orgName, this.projectName).subscribe({
      next: (project) => {
        this.project.set(project);
        if (showLoading) this.loading.set(false);
        this.onProjectDataLoaded();
        this.loadEntryPoints(project);
      },
      error: (error) => {
        console.error('Failed to load project:', error);
        if (showLoading) this.loading.set(false);
      },
    });
  }

  private readonly buildStatusOrder: Record<string, number> = {
    Building: 0,
    Queued: 1,
    FailedPermanent: 2,
    FailedTimeout: 2,
    FailedTransient: 2,
    Aborted: 3,
    Completed: 4,
    Substituted: 4,
  };

  private readonly statusesWithBuilds = new Set<EvaluationStatus>(['Building', 'Waiting', 'Completed', 'Failed', 'Aborted']);

  loadEntryPoints(project?: ProjectDetail): void {
    // When the newest eval is Queued/Evaluating it has no builds yet - fall back to
    // the most recent evaluation that actually has entry points.
    const evalWithBuilds = project?.last_evaluations?.find(e => this.statusesWithBuilds.has(e.status));
    this.projectsService.getEntryPoints(this.orgName, this.projectName, evalWithBuilds?.id).subscribe({
      next: (eps) => this.entryPoints.set(
        [...eps].sort((a, b) => {
          const oa = this.buildStatusOrder[a.build_status] ?? 99;
          const ob = this.buildStatusOrder[b.build_status] ?? 99;
          if (oa !== ob) return oa - ob;
          return this.getDerivationName(a.derivation_path).localeCompare(this.getDerivationName(b.derivation_path));
        })
      ),
      error: (error) => console.error('Failed to load entry points:', error),
    });
  }

  startEvaluation(): void {
    this.starting.set(true);
    this.errorMessage.set(null);
    this.projectsService.startEvaluation(this.orgName, this.projectName).subscribe({
      next: () => {
        // Keep starting=true; polling will clear it once the evaluation appears
        this.loadProjectData(false);
      },
      error: (error) => {
        console.error('Failed to start evaluation:', error);
        this.errorMessage.set(error?.message || 'Failed to start evaluation.');
        this.starting.set(false);
      },
    });
  }

  restartFailedBuilds(): void {
    this.starting.set(true);
    this.errorMessage.set(null);
    this.projectsService.restartFailedBuilds(this.orgName, this.projectName).subscribe({
      next: () => {
        this.loadProjectData(false);
      },
      error: (error) => {
        console.error('Failed to restart failed builds:', error);
        this.errorMessage.set(error?.message || 'Failed to restart failed builds.');
        this.starting.set(false);
      },
    });
  }

  dismissError(): void {
    this.errorMessage.set(null);
  }

  abortEvaluation(evaluationId: string): void {
    this.projectsService.abortEvaluation(this.orgName, this.projectName, evaluationId).subscribe({
      next: () => {
        this.loadProjectData(false);
      },
      error: (error) => {
        console.error('Failed to abort evaluation:', error);
      },
    });
  }

  startLiveUpdates(): void {
    this.liveSub = this.live
      .connect(`/projects/${this.orgName}/${this.projectName}/live`)
      .pipe(auditTime(500))
      .subscribe(() => this.loadProjectData(false));
  }

  onProjectDataLoaded(): void {
    if (this.starting()) {
      const hasRunning = this.project()?.last_evaluations?.some(e => this.isRunningStatus(e.status)) ?? false;
      if (hasRunning) {
        this.starting.set(false);
      }
    }
  }

  stopLiveUpdates(): void {
    this.liveSub?.unsubscribe();
    this.tickSubscription?.unsubscribe();
  }

  isRunningStatus(status: EvaluationStatus): boolean {
    return isRunningEvaluationStatus(status);
  }

  isBuildRunning(status: BuildStatus): boolean {
    return status === 'Queued' || status === 'Building';
  }

  formatBuildStatus(status: BuildStatus): string {
    if (status === 'DependencyFailed') return 'Dependency Failed';
    if (status === 'FailedPermanent' || status === 'FailedTransient' || status === 'FailedTimeout') return 'Failed';
    return status;
  }

  getBuildStatusClass(status: BuildStatus): string {
    switch (status) {
      case 'Completed': case 'Substituted': return 'status-success';
      case 'FailedPermanent': case 'FailedTransient': case 'FailedTimeout': return 'status-danger';
      case 'Aborted': case 'DependencyFailed': return 'status-secondary';
      case 'Queued': case 'Building': return 'status-running';
      default: return '';
    }
  }

  getBuildStatusIcon(status: BuildStatus): string {
    switch (status) {
      case 'Completed': case 'Substituted': return 'check_circle';
      case 'FailedPermanent': case 'FailedTransient': case 'FailedTimeout': return 'error';
      case 'Aborted': case 'DependencyFailed': return 'cancel';
      case 'Queued': return 'schedule';
      case 'Building': return 'sync';
      default: return 'help';
    }
  }

  effectiveEntryStatusClass(ep: EntryPointSummary): string {
    return this.getBuildStatusClass(ep.build_status);
  }

  effectiveEntryStatusIcon(ep: EntryPointSummary): string {
    return this.getBuildStatusIcon(ep.build_status);
  }

  effectiveEntryStatusLabel(ep: EntryPointSummary): string {
    return this.formatBuildStatus(ep.build_status);
  }

  getEvaluationDuration(evaluation: EvaluationSummary): string {
    const start = parseUtcTimestamp(evaluation.created_at);
    const end = this.isRunningStatus(evaluation.status)
      ? this.tick()
      : parseUtcTimestamp(evaluation.updated_at);
    return formatEvaluationDuration(end - start);
  }

  formatArchitecture(arch: string | undefined): string {
    if (!arch) return '';
    return arch
      .replace('X86_64Linux', 'x86_64-linux')
      .replace('Aarch64Linux', 'aarch64-linux')
      .replace('X86_64Darwin', 'x86_64-darwin')
      .replace('Aarch64Darwin', 'aarch64-darwin')
      .replace('BUILTIN', 'builtin');
  }

  getDerivationName(path: string): string {
    const parts = path.split('/').pop() ?? path;
    // Strip the nix store hash prefix and .drv extension (e.g. "abc123xyz-name.drv" -> "name")
    const match = parts.match(/^[a-z0-9]+-(.+?)(?:\.drv)?$/);
    return match ? match[1] : parts;
  }

  getStatusClass(status: EvaluationStatus): string {
    switch (status) {
      case 'Completed': return 'status-success';
      case 'Failed': return 'status-danger';
      case 'Aborted': return 'status-secondary';
      case 'Waiting': return 'status-warning';
      case 'Queued': case 'Fetching': case 'EvaluatingFlake': case 'EvaluatingDerivation': case 'Building': return 'status-running';
      default: return '';
    }
  }

  getStatusIcon(status: EvaluationStatus): string {
    switch (status) {
      case 'Completed': return 'check_circle';
      case 'Failed': return 'error';
      case 'Aborted': return 'cancel';
      case 'Queued': return 'schedule';
      case 'Waiting': return 'pause_circle';
      case 'Fetching': return 'cloud_download';
      case 'EvaluatingFlake': case 'EvaluatingDerivation': case 'Building': return 'sync';
      default: return 'help';
    }
  }

  getTriggerLabel(type: TriggerType | null): string {
    switch (type) {
      case 'polling': return 'Polling';
      case 'reporter_push': return 'Push';
      case 'reporter_pull_request': return 'PR';
      case 'time': return 'Schedule';
      default: return 'Manual';
    }
  }

  getStatusLabel(status: EvaluationStatus): string {
    switch (status) {
      case 'Fetching': return 'Fetching';
      case 'EvaluatingFlake': case 'EvaluatingDerivation': return 'Evaluating';
      default: return status;
    }
  }
}
