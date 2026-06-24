/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnDestroy, OnInit, inject, signal } from '@angular/core';
import { CommonModule } from '@angular/common';
import { ActivatedRoute, RouterModule } from '@angular/router';
import { FormsModule } from '@angular/forms';
import { Subject, forkJoin, EMPTY } from 'rxjs';
import { debounceTime, switchMap } from 'rxjs/operators';
import { DialogModule } from 'primeng/dialog';
import { ButtonModule } from 'primeng/button';
import { InputTextModule } from 'primeng/inputtext';
import { TextareaModule } from 'primeng/textarea';
import { AuthService } from '@core/services/auth.service';
import { OrganizationsService } from '@core/services/organizations.service';
import { ProjectsService } from '@core/services/projects.service';
import { LoadingSpinnerComponent } from '@shared/components/loading-spinner/loading-spinner.component';
import { EmptyStateComponent } from '@shared/components/empty-state/empty-state.component';
import { LabelHelpComponent } from '@shared/components/form';
import { EvalStatusBadgeComponent } from '@shared/components/eval-status-badge/eval-status-badge.component';
import { slugify } from '@shared/text';
import { Organization, Project } from '@core/models';

const RESERVED_PROJECT_NAMES = ['build-request'];

@Component({
  selector: 'app-organization-detail',
  standalone: true,
  imports: [
    CommonModule,
    RouterModule,
    FormsModule,
    DialogModule,
    ButtonModule,
    InputTextModule,
    TextareaModule,
    LoadingSpinnerComponent,
    EmptyStateComponent,
    LabelHelpComponent,
    EvalStatusBadgeComponent,
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
  nameCheckState = signal<'idle' | 'invalid' | 'reserved' | 'checking' | 'available' | 'taken'>('idle');

  orgName = '';

  newProject = {
    name: '',
    display_name: '',
    description: '',
    repository: '',
    wildcard: 'packages.x86_64-linux.*',
  };

  protected projectNameEditedByUser = false;

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
    this.newProject = { name: '', display_name: '', description: '', repository: '', wildcard: 'packages.x86_64-linux.*' };
    this.projectNameEditedByUser = false;
    this.nameCheckState.set('idle');
    this.createError.set(null);
    this.showCreateDialog.set(true);
  }

  onProjectDisplayNameChange(value: string): void {
    if (!this.projectNameEditedByUser) {
      const slug = slugify(value);
      this.newProject.name = slug;
      this.onProjectNameChange(slug);
    }
  }

  onProjectNameUserInput(): void {
    this.projectNameEditedByUser = true;
  }

  onProjectNameChange(name: string): void {
    if (!name) { this.nameCheckState.set('idle'); this.nameCheck$.next(''); return; }
    if (!/^[a-z0-9]([a-z0-9-]*[a-z0-9])?$/.test(name)) {
      this.nameCheckState.set('invalid');
      this.nameCheck$.next(''); // cancel any pending debounce without making an API call
      return;
    }
    if (RESERVED_PROJECT_NAMES.includes(name.toLowerCase())) {
      this.nameCheckState.set('reserved');
      this.nameCheck$.next('');
      return;
    }
    this.nameCheckState.set('checking');
    this.nameCheck$.next(name);
  }

  get wildcardInvalid(): boolean {
    const w = this.newProject.wildcard.trim();
    if (!w) return false; // empty means use default - not invalid
    const parts = w.split(',').map((p) => p.trim());
    return parts.some((p) => !p || p.startsWith('.') || /\s/.test(p));
  }

  createProject(): void {
    if (!this.newProject.name || !this.newProject.display_name || !this.newProject.repository) {
      return;
    }
    if (RESERVED_PROJECT_NAMES.includes(this.newProject.name.trim().toLowerCase())) {
      this.nameCheckState.set('reserved');
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
