/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, inject, signal, ChangeDetectionStrategy } from '@angular/core';
import { CommonModule } from '@angular/common';
import { ActivatedRoute, Router, RouterModule } from '@angular/router';
import { FormsModule } from '@angular/forms';
import { ProjectsService } from '@core/services/projects.service';
import { ProjectAccessService } from '@core/services/project-access.service';
import { LoadingSpinnerComponent } from '@shared/ui';
import { WritableDirective, ManagedDisableDirective } from '@shared/access';
import { Project, AccessState } from '@core/models';
import { ButtonComponent, CheckboxComponent, DialogComponent, DividerComponent, InputDirective } from '@shared/ui';

@Component({
  selector: 'app-project-settings',
  standalone: true,
  imports: [
    CommonModule,
    RouterModule,
    FormsModule,
    DialogComponent,
    DividerComponent,
    ButtonComponent,
    InputDirective,
    InputDirective,
    CheckboxComponent,
    LoadingSpinnerComponent,
    WritableDirective,
    ManagedDisableDirective,
  ],
  templateUrl: './project-settings.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './project-settings.component.scss',
})
export class ProjectSettingsComponent implements OnInit {
  private route = inject(ActivatedRoute);
  private router = inject(Router);
  private projectsService = inject(ProjectsService);
  private projectAccess = inject(ProjectAccessService);

  access = signal<AccessState>({ managed: false, canEdit: false, canTrigger: false });

  loading = signal(true);
  saving = signal(false);
  deleting = signal(false);
  sshLoading = signal(true);
  generatingSSH = signal(false);

  project = signal<Project | null>(null);
  sshKey = signal<string>('');

  showDeleteDialog = signal(false);
  showRegenerateKeyDialog = signal(false);
  saveError = signal<string | null>(null);
  saveSuccess = signal(false);

  projectName = '';

  formData = {
    display_name: '',
    description: '',
    public: false,
    hide_build_requests: false,
  };

  ngOnInit(): void {
    this.projectName = this.route.snapshot.paramMap.get('project') || '';
    this.projectAccess.forProject(this.projectName).then((s) => this.access.set(s));
    this.loadProject();
    this.loadSSHKey();
  }

  loadProject(): void {
    this.loading.set(true);
    this.projectsService.getProject(this.projectName).subscribe({
      next: (project) => {
        this.project.set(project);
        this.formData = {
          display_name: project.display_name,
          description: project.description,
          public: project.public,
          hide_build_requests: project.hide_build_requests,
        };
        this.loading.set(false);
      },
      error: (error) => {
        console.error('Failed to load project:', error);
        this.loading.set(false);
      },
    });
  }

  loadSSHKey(): void {
    this.sshLoading.set(true);
    this.projectsService.getSSHKey(this.projectName).subscribe({
      next: (key) => {
        this.sshKey.set(key);
        this.sshLoading.set(false);
      },
      error: (error) => {
        console.error('Failed to load SSH key:', error);
        this.sshLoading.set(false);
      },
    });
  }

  saveSettings(): void {
    this.saving.set(true);
    this.saveError.set(null);
    this.saveSuccess.set(false);
    const visibilityCall = this.formData.public
      ? this.projectsService.setPublic(this.projectName)
      : this.projectsService.setPrivate(this.projectName);

    this.projectsService.updateProject(this.projectName, {
      display_name: this.formData.display_name,
      description: this.formData.description,
      hide_build_requests: this.formData.hide_build_requests,
    }).subscribe({
      next: () => {
        visibilityCall.subscribe({
          next: () => {
            this.saving.set(false);
            this.saveSuccess.set(true);
            this.loadProject();
          },
          error: (error) => {
            this.saveError.set(error?.message || 'Failed to update visibility.');
            this.saving.set(false);
            this.loadProject();
          },
        });
      },
      error: (error) => {
        this.saveError.set(error?.message || 'Failed to save settings.');
        this.saving.set(false);
      },
    });
  }

  deleteProject(): void {
    this.deleting.set(true);
    this.projectsService.deleteProject(this.projectName).subscribe({
      next: () => {
        this.router.navigate(['/projects']);
      },
      error: (error) => {
        console.error('Failed to delete project:', error);
        this.deleting.set(false);
        this.showDeleteDialog.set(false);
      },
    });
  }

  confirmRegenerateSSHKey(): void {
    this.showRegenerateKeyDialog.set(false);
    this.generateSSHKey();
  }

  generateSSHKey(): void {
    this.generatingSSH.set(true);
    this.projectsService.generateSSHKey(this.projectName).subscribe({
      next: (key) => {
        this.sshKey.set(key);
        this.generatingSSH.set(false);
      },
      error: (error) => {
        console.error('Failed to generate SSH key:', error);
        this.generatingSSH.set(false);
      },
    });
  }

  copySSHKey(): void {
    navigator.clipboard.writeText(this.sshKey());
  }
}
