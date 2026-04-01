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
import { UserService } from '@core/services/user.service';
import { AutoCompleteModule } from 'primeng/autocomplete';
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
    LoadingSpinnerComponent,
  ],
  templateUrl: './project-settings.component.html',
  styleUrl: './project-settings.component.scss',
})
export class ProjectSettingsComponent implements OnInit {
  private route = inject(ActivatedRoute);
  private router = inject(Router);
  private projectsService = inject(ProjectsService);
  private userService = inject(UserService);

  loading = signal(true);
  saving = signal(false);
  deleting = signal(false);
  toggling = signal(false);
  transferring = signal(false);

  project = signal<Project | null>(null);
  showDeleteDialog = signal(false);
  errorMessage = signal<string | null>(null);
  saveSuccess = signal(false);
  transferUsername = '';
  transferError = signal<string | null>(null);
  transferSuccess = signal(false);
  transferUserSuggestions = signal<string[]>([]);

  orgName = '';
  projectName = '';

  formData = {
    display_name: '',
    description: '',
    repository: '',
    evaluation_wildcard: '',
  };

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

  onTransferUserSearch(event: { query: string }): void {
    if (!event.query.trim()) {
      this.transferUserSuggestions.set([]);
      return;
    }
    this.userService.searchUsers(event.query).subscribe({
      next: (users) => this.transferUserSuggestions.set(users.map((u) => u.username)),
      error: () => this.transferUserSuggestions.set([]),
    });
  }

  transferOwnership(): void {
    if (!this.transferUsername.trim()) return;
    this.transferring.set(true);
    this.transferError.set(null);
    this.transferSuccess.set(false);
    this.projectsService.transferOwnership(this.orgName, this.projectName, this.transferUsername.trim()).subscribe({
      next: () => {
        this.transferring.set(false);
        this.transferSuccess.set(true);
        this.transferUsername = '';
        this.loadProject();
      },
      error: (error) => {
        this.transferError.set(error.message || 'Failed to transfer ownership.');
        this.transferring.set(false);
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
