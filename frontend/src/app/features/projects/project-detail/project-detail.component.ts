/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, OnDestroy, inject, signal } from '@angular/core';
import { CommonModule } from '@angular/common';
import { ActivatedRoute, RouterModule } from '@angular/router';
import { interval, Subscription } from 'rxjs';
import { ButtonModule } from 'primeng/button';
import { ProjectsService } from '@core/services/projects.service';
import { LoadingSpinnerComponent } from '@shared/components/loading-spinner/loading-spinner.component';
import { EmptyStateComponent } from '@shared/components/empty-state/empty-state.component';
import { ProjectDetail, EvaluationSummary, EvaluationStatus } from '@core/models';

@Component({
  selector: 'app-project-detail',
  standalone: true,
  imports: [
    CommonModule,
    RouterModule,
    ButtonModule,
    LoadingSpinnerComponent,
    EmptyStateComponent,
  ],
  templateUrl: './project-detail.component.html',
  styleUrl: './project-detail.component.scss',
})
export class ProjectDetailComponent implements OnInit, OnDestroy {
  private route = inject(ActivatedRoute);
  private projectsService = inject(ProjectsService);

  loading = signal(true);
  project = signal<ProjectDetail | null>(null);
  starting = signal(false);

  orgName = '';
  projectName = '';

  private pollSubscription?: Subscription;

  ngOnInit(): void {
    this.orgName = this.route.snapshot.paramMap.get('org') || '';
    this.projectName = this.route.snapshot.paramMap.get('project') || '';
    this.loadProjectData();
    this.startPolling();
  }

  ngOnDestroy(): void {
    this.stopPolling();
  }

  loadProjectData(): void {
    this.loading.set(true);
    this.projectsService.getProject(this.orgName, this.projectName).subscribe({
      next: (project) => {
        this.project.set(project);
        this.loading.set(false);
      },
      error: (error) => {
        console.error('Failed to load project:', error);
        this.loading.set(false);
      },
    });
  }

  startEvaluation(): void {
    this.starting.set(true);
    this.projectsService.startEvaluation(this.orgName, this.projectName).subscribe({
      next: () => {
        this.starting.set(false);
        this.loadProjectData();
      },
      error: (error) => {
        console.error('Failed to start evaluation:', error);
        this.starting.set(false);
      },
    });
  }

  abortEvaluation(evaluationId: string): void {
    this.projectsService.abortEvaluation(this.orgName, this.projectName, evaluationId).subscribe({
      next: () => {
        this.loadProjectData();
      },
      error: (error) => {
        console.error('Failed to abort evaluation:', error);
      },
    });
  }

  startPolling(): void {
    this.pollSubscription = interval(3000).subscribe(() => {
      const proj = this.project();
      if (proj?.last_evaluations?.some(e => this.isRunningStatus(e.status))) {
        this.loadProjectData();
      }
    });
  }

  stopPolling(): void {
    this.pollSubscription?.unsubscribe();
  }

  isRunningStatus(status: EvaluationStatus): boolean {
    return status === 'Queued' || status === 'Evaluating' || status === 'Building';
  }

  getStatusClass(status: EvaluationStatus): string {
    switch (status) {
      case 'Completed': return 'status-success';
      case 'Failed': return 'status-danger';
      case 'Aborted': return 'status-warning';
      case 'Queued': case 'Evaluating': case 'Building': return 'status-running';
      default: return '';
    }
  }

  getStatusIcon(status: EvaluationStatus): string {
    switch (status) {
      case 'Completed': return 'check_circle';
      case 'Failed': return 'error';
      case 'Aborted': return 'cancel';
      case 'Queued': return 'schedule';
      case 'Evaluating': case 'Building': return 'sync';
      default: return 'help';
    }
  }
}
