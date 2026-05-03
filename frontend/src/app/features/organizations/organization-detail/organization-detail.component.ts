/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnDestroy, OnInit, computed, inject, signal } from '@angular/core';
import { CommonModule } from '@angular/common';
import { ActivatedRoute, RouterModule } from '@angular/router';
import { FormsModule } from '@angular/forms';
import { Subject, forkJoin, EMPTY } from 'rxjs';
import { debounceTime, switchMap } from 'rxjs/operators';
import { ButtonModule } from 'primeng/button';
import { InputTextModule } from 'primeng/inputtext';
import { TextareaModule } from 'primeng/textarea';
import { AuthService } from '@core/services/auth.service';
import { OrganizationsService } from '@core/services/organizations.service';
import { ProjectsService } from '@core/services/projects.service';
import { LoadingSpinnerComponent } from '@shared/components/loading-spinner/loading-spinner.component';
import { EmptyStateComponent } from '@shared/components/empty-state/empty-state.component';
import { PageLayoutComponent, SettingsSectionComponent } from '@shared/components/layout';
import {
  FormDialogComponent,
  FormFieldComponent,
  LabelHelpComponent,
  MessageBannerComponent,
} from '@shared/components/form';
import { Organization, Project, EvaluationStatus } from '@core/models';

@Component({
  selector: 'app-organization-detail',
  standalone: true,
  imports: [
    CommonModule,
    RouterModule,
    FormsModule,
    ButtonModule,
    InputTextModule,
    TextareaModule,
    LoadingSpinnerComponent,
    EmptyStateComponent,
    PageLayoutComponent,
    SettingsSectionComponent,
    FormDialogComponent,
    FormFieldComponent,
    LabelHelpComponent,
    MessageBannerComponent,
  ],
  templateUrl: './organization-detail.component.html',
  styleUrl: './organization-detail.component.scss',
})
export class OrganizationDetailComponent implements OnInit, OnDestroy {
  private route = inject(ActivatedRoute);
  protected authService = inject(AuthService);
  private organizationsService = inject(OrganizationsService);
  private projectsService = inject(ProjectsService);
  private nameCheck$ = new Subject<string>();

  loading = signal(true);
  organization = signal<Organization | null>(null);
  projects = signal<Project[]>([]);
  projectsTotal = signal(0);
  projectsPage = signal(1);
  showCreateDialog = signal(false);
  creating = signal(false);
  createError = signal<string | null>(null);
  nameCheckState = signal<'idle' | 'invalid' | 'checking' | 'available' | 'taken'>('idle');

  orgName = '';

  newProject = {
    name: '',
    display_name: '',
    description: '',
    repository: '',
    evaluation_wildcard: 'packages.x86_64-linux.*',
  };

  protected projectNameEditedByUser = false;

  readonly nameCheckHint = computed(() => {
    switch (this.nameCheckState()) {
      case 'taken': return 'This name is already taken.';
      case 'invalid': return 'Only lowercase letters, numbers, and hyphens. Cannot start or end with a hyphen.';
      default: return 'Lowercase letters, numbers, and hyphens only.';
    }
  });

  wildcardHint(): string {
    return this.wildcardInvalid
      ? 'Each pattern must be non-empty, not start with a period, and contain no spaces.'
      : 'Pattern for selecting which outputs to evaluate (default: packages.x86_64-linux.*).';
  }

  ngOnInit(): void {
    this.orgName = this.route.snapshot.paramMap.get('org') || '';
    this.loadOrganizationData();
    this.nameCheck$.pipe(
      debounceTime(400),
      switchMap((name) => name ? this.projectsService.checkProjectNameAvailable(this.orgName, name) : EMPTY),
    ).subscribe((available) => {
      this.nameCheckState.set(available ? 'available' : 'taken');
    });
  }

  ngOnDestroy(): void {
    this.nameCheck$.complete();
  }

  loadOrganizationData(): void {
    this.loading.set(true);

    forkJoin({
      organization: this.organizationsService.getOrganization(this.orgName),
      projects: this.projectsService.getProjects(this.orgName, this.projectsPage()),
    }).subscribe({
      next: ({ organization, projects }) => {
        this.organization.set(organization);
        this.projects.set(projects.items);
        this.projectsTotal.set(projects.total);
        this.projectsPage.set(projects.page);
        this.loading.set(false);
      },
      error: (error) => {
        console.error('Failed to load organization data:', error);
        this.loading.set(false);
      },
    });
  }

  openCreateDialog(): void {
    this.newProject = { name: '', display_name: '', description: '', repository: '', evaluation_wildcard: 'packages.x86_64-linux.*' };
    this.projectNameEditedByUser = false;
    this.nameCheckState.set('idle');
    this.createError.set(null);
    this.showCreateDialog.set(true);
  }

  onProjectDisplayNameChange(value: string): void {
    if (!this.projectNameEditedByUser) {
      const slug = this.toSlug(value);
      this.newProject.name = slug;
      this.onProjectNameChange(slug);
    }
  }

  onProjectNameUserInput(): void {
    this.projectNameEditedByUser = true;
  }

  private toSlug(text: string): string {
    return text
      .toLowerCase()
      .replace(/[^a-z0-9]+/g, '-')
      .replace(/^-+|-+$/g, '');
  }

  onProjectNameChange(name: string): void {
    if (!name) { this.nameCheckState.set('idle'); this.nameCheck$.next(''); return; }
    if (!/^[a-z0-9]([a-z0-9-]*[a-z0-9])?$/.test(name)) {
      this.nameCheckState.set('invalid');
      this.nameCheck$.next('');
      return;
    }
    this.nameCheckState.set('checking');
    this.nameCheck$.next(name);
  }

  isRunningStatus(status: EvaluationStatus): boolean {
    return status === 'Queued' || status === 'Fetching' || status === 'EvaluatingFlake' || status === 'EvaluatingDerivation' || status === 'Building' || status === 'Waiting';
  }

  getEvalStatusClass(status: EvaluationStatus): string {
    switch (status) {
      case 'Completed': return 'status-success';
      case 'Failed': return 'status-danger';
      case 'Aborted': return 'status-secondary';
      case 'Waiting': return 'status-warning';
      default: return 'status-running';
    }
  }

  getEvalStatusIcon(status: EvaluationStatus): string {
    switch (status) {
      case 'Completed': return 'check_circle';
      case 'Failed': return 'error';
      case 'Aborted': return 'cancel';
      case 'Queued': return 'hourglass_empty';
      case 'Waiting': return 'pause_circle';
      default: return 'sync';
    }
  }

  getEvalStatusLabel(status: EvaluationStatus): string {
    if (status === 'Fetching') return 'Fetching';
    if (status === 'EvaluatingFlake' || status === 'EvaluatingDerivation') return 'Evaluating';
    return status;
  }

  get wildcardInvalid(): boolean {
    const w = this.newProject.evaluation_wildcard.trim();
    if (!w) return false;
    const parts = w.split(',').map((p) => p.trim());
    return parts.some((p) => !p || p.startsWith('.') || /\s/.test(p));
  }

  createProject(): void {
    if (!this.newProject.name || !this.newProject.display_name || !this.newProject.repository) {
      return;
    }

    this.creating.set(true);
    this.createError.set(null);
    this.projectsService
      .createProject(this.orgName, this.newProject)
      .subscribe({
        next: () => {
          this.creating.set(false);
          this.showCreateDialog.set(false);
          this.loadOrganizationData();
        },
        error: (error) => {
          this.createError.set(error?.message || 'Failed to create project.');
          this.creating.set(false);
        },
      });
  }
}
