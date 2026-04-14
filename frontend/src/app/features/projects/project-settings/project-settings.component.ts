/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, inject, signal } from '@angular/core';
import { CommonModule } from '@angular/common';
import { ActivatedRoute, Router, RouterModule } from '@angular/router';
import { FormsModule } from '@angular/forms';
import { DialogModule } from 'primeng/dialog';
import { ButtonModule } from 'primeng/button';
import { InputTextModule } from 'primeng/inputtext';
import { TextareaModule } from 'primeng/textarea';
import { ProjectsService } from '@core/services/projects.service';
import { OrganizationsService } from '@core/services/organizations.service';
import { AutoCompleteModule } from 'primeng/autocomplete';
import { SelectModule } from 'primeng/select';
import { LoadingSpinnerComponent } from '@shared/components/loading-spinner/loading-spinner.component';
import { Project } from '@core/models';

@Component({
  selector: 'app-project-settings',
  standalone: true,
  imports: [
    CommonModule,
    RouterModule,
    FormsModule,
    DialogModule,
    ButtonModule,
    InputTextModule,
    TextareaModule,
    AutoCompleteModule,
    SelectModule,
    LoadingSpinnerComponent,
  ],
  templateUrl: './project-settings.component.html',
  styleUrl: './project-settings.component.scss',
})
export class ProjectSettingsComponent implements OnInit {
  private route = inject(ActivatedRoute);
  private router = inject(Router);
  private projectsService = inject(ProjectsService);
  private orgsService = inject(OrganizationsService);

  loading = signal(true);
  saving = signal(false);
  deleting = signal(false);
  toggling = signal(false);
  transferring = signal(false);

  project = signal<Project | null>(null);
  showDeleteDialog = signal(false);
  showTransferDialog = signal(false);
  errorMessage = signal<string | null>(null);
  saveSuccess = signal(false);
  transferOrgName = '';
  transferError = signal<string | null>(null);
  transferSuccess = signal(false);
  transferOrgSuggestions = signal<string[]>([]);

  orgName = '';
  projectName = '';

  formData = {
    display_name: '',
    description: '',
    repository: '',
    evaluation_wildcard: '',
    keep_evaluations: 30,
  };

  ciProviders = [
    { label: 'None', value: '' },
    { label: 'Gitea', value: 'gitea' },
    { label: 'GitHub', value: 'github' },
  ];

  ciFormData = {
    ci_reporter_type: '',
    ci_reporter_url: '',
    ci_reporter_token: '',
  };

  savingCi = signal(false);
  removingCi = signal(false);
  ciSaveSuccess = signal(false);
  ciErrorMessage = signal<string | null>(null);

  ngOnInit(): void {
    this.orgName = this.route.snapshot.paramMap.get('org') || '';
    this.projectName = this.route.snapshot.paramMap.get('project') || '';
    this.loadProject();
  }

  loadProject(): void {
    this.loading.set(true);
    this.projectsService.getProjectInfo(this.orgName, this.projectName).subscribe({
      next: (project) => {
        this.project.set(project);
        this.formData = {
          display_name: project.display_name,
          description: project.description,
          repository: project.repository,
          evaluation_wildcard: project.evaluation_wildcard,
          keep_evaluations: project.keep_evaluations,
        };
        this.ciFormData = {
          ci_reporter_type: project.ci_reporter_type ?? '',
          ci_reporter_url: project.ci_reporter_url ?? '',
          ci_reporter_token: '',
        };
        this.loading.set(false);
      },
      error: (error) => {
        console.error('Failed to load project:', error);
        this.loading.set(false);
      },
    });
  }

  saveSettings(): void {
    this.saving.set(true);
    this.errorMessage.set(null);
    this.saveSuccess.set(false);
    this.projectsService.updateProject(this.orgName, this.projectName, this.formData).subscribe({
      next: () => {
        this.saving.set(false);
        this.saveSuccess.set(true);
        this.loadProject();
      },
      error: (error) => {
        this.errorMessage.set(error.message || 'Failed to save settings.');
        this.saving.set(false);
      },
    });
  }

  toggleActive(): void {
    const proj = this.project();
    if (!proj) return;

    this.toggling.set(true);
    const action = proj.active
      ? this.projectsService.deactivateProject(this.orgName, this.projectName)
      : this.projectsService.activateProject(this.orgName, this.projectName);

    action.subscribe({
      next: () => {
        this.toggling.set(false);
        this.loadProject();
      },
      error: (error) => {
        console.error('Failed to toggle project status:', error);
        this.toggling.set(false);
      },
    });
  }

  onTransferOrgSearch(event: { query: string }): void {
    const q = event.query.trim().toLowerCase();
    this.orgsService.getOrganizations().subscribe({
      next: (res) => {
        const names = res.items
          .map((o) => o.name)
          .filter((n) => n !== this.orgName && (!q || n.toLowerCase().includes(q)));
        this.transferOrgSuggestions.set(names);
      },
      error: () => this.transferOrgSuggestions.set([]),
    });
  }

  onTransferDialogHide(): void {
    this.transferOrgName = '';
    this.transferError.set(null);
    this.transferSuccess.set(false);
  }

  transferOwnership(): void {
    if (!this.transferOrgName.trim()) return;
    this.transferring.set(true);
    this.transferError.set(null);
    this.transferSuccess.set(false);
    this.projectsService.transferOwnership(this.orgName, this.projectName, this.transferOrgName.trim()).subscribe({
      next: () => {
        this.transferring.set(false);
        this.transferSuccess.set(true);
        const targetOrg = this.transferOrgName.trim();
        this.transferOrgName = '';
        setTimeout(() => {
          this.showTransferDialog.set(false);
          this.router.navigate(['/organization', targetOrg]);
        }, 1500);
      },
      error: (error) => {
        this.transferError.set(error.message || 'Failed to transfer ownership.');
        this.transferring.set(false);
      },
    });
  }

  saveCiSettings(): void {
    this.savingCi.set(true);
    this.ciErrorMessage.set(null);
    this.ciSaveSuccess.set(false);

    const patch: Record<string, string> = {
      ci_reporter_type: this.ciFormData.ci_reporter_type,
      ci_reporter_url: this.ciFormData.ci_reporter_url,
    };
    if (this.ciFormData.ci_reporter_token.trim()) {
      patch['ci_reporter_token'] = this.ciFormData.ci_reporter_token;
    }

    this.projectsService.updateProject(this.orgName, this.projectName, patch as any).subscribe({
      next: () => {
        this.savingCi.set(false);
        this.ciSaveSuccess.set(true);
        this.ciFormData.ci_reporter_token = '';
        this.loadProject();
      },
      error: (error) => {
        this.ciErrorMessage.set(error.message || 'Failed to save integration settings.');
        this.savingCi.set(false);
      },
    });
  }

  removeCiIntegration(): void {
    this.removingCi.set(true);
    this.ciErrorMessage.set(null);
    this.ciSaveSuccess.set(false);
    this.projectsService.removeIntegration(this.orgName, this.projectName).subscribe({
      next: () => {
        this.removingCi.set(false);
        this.ciSaveSuccess.set(true);
        this.loadProject();
      },
      error: (error) => {
        this.ciErrorMessage.set(error.message || 'Failed to remove integration.');
        this.removingCi.set(false);
      },
    });
  }

  deleteProject(): void {
    this.deleting.set(true);
    this.projectsService.deleteProject(this.orgName, this.projectName).subscribe({
      next: () => {
        this.router.navigate(['/organization', this.orgName]);
      },
      error: (error) => {
        console.error('Failed to delete project:', error);
        this.deleting.set(false);
        this.showDeleteDialog.set(false);
      },
    });
  }
}
