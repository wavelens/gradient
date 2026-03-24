/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, inject, signal } from '@angular/core';
import { CommonModule } from '@angular/common';
import { ActivatedRoute, RouterModule } from '@angular/router';
import { FormsModule } from '@angular/forms';
import { forkJoin } from 'rxjs';
import { DialogModule } from 'primeng/dialog';
import { ButtonModule } from 'primeng/button';
import { InputTextModule } from 'primeng/inputtext';
import { TextareaModule } from 'primeng/textarea';
import { OrganizationsService } from '@core/services/organizations.service';
import { ProjectsService } from '@core/services/projects.service';
import { LoadingSpinnerComponent } from '@shared/components/loading-spinner/loading-spinner.component';
import { EmptyStateComponent } from '@shared/components/empty-state/empty-state.component';
import { Organization, Project } from '@core/models';

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
  ],
  templateUrl: './organization-detail.component.html',
  styleUrl: './organization-detail.component.scss',
})
export class OrganizationDetailComponent implements OnInit {
  private route = inject(ActivatedRoute);
  private organizationsService = inject(OrganizationsService);
  private projectsService = inject(ProjectsService);

  loading = signal(true);
  organization = signal<Organization | null>(null);
  projects = signal<Project[]>([]);
  showCreateDialog = signal(false);
  creating = signal(false);

  orgName = '';

  newProject = {
    name: '',
    display_name: '',
    description: '',
    repository: '',
    evaluation_wildcard: '**',
  };

  ngOnInit(): void {
    this.orgName = this.route.snapshot.paramMap.get('org') || '';
    this.loadOrganizationData();
  }

  loadOrganizationData(): void {
    this.loading.set(true);

    forkJoin({
      organization: this.organizationsService.getOrganization(this.orgName),
      projects: this.projectsService.getProjects(this.orgName),
    }).subscribe({
      next: ({ organization, projects }) => {
        this.organization.set(organization);
        this.projects.set(projects);
        this.loading.set(false);
      },
      error: (error) => {
        console.error('Failed to load organization data:', error);
        this.loading.set(false);
      },
    });
  }

  openCreateDialog(): void {
    this.newProject = {
      name: '',
      display_name: '',
      description: '',
      repository: '',
      evaluation_wildcard: '**',
    };
    this.showCreateDialog.set(true);
  }

  createProject(): void {
    if (!this.newProject.name || !this.newProject.display_name || !this.newProject.repository) {
      return;
    }

    this.creating.set(true);
    this.projectsService
      .createProject(this.orgName, this.newProject)
      .subscribe({
        next: () => {
          this.creating.set(false);
          this.showCreateDialog.set(false);
          this.loadOrganizationData();
        },
        error: (error) => {
          console.error('Failed to create project:', error);
          this.creating.set(false);
        },
      });
  }

}
