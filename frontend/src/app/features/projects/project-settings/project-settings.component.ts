/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, computed, inject, signal, ChangeDetectionStrategy } from '@angular/core';
import { CommonModule } from '@angular/common';
import { ActivatedRoute, Router, RouterModule } from '@angular/router';
import { FormsModule } from '@angular/forms';
import { Observable, finalize, tap } from 'rxjs';
import { BreadcrumbsService } from '@core/services/breadcrumbs.service';
import { ConfigService } from '@core/services/config.service';
import { ProjectsService } from '@core/services/projects.service';
import { ProjectAccessService } from '@core/services/project-access.service';
import { NotifyService } from '@core/services/notify.service';
import { apiErrorMessage } from '@core/services/api.service';
import {
  ButtonComponent,
  CheckboxComponent,
  CopyFieldComponent,
  DialogComponent,
  FormFieldComponent,
  InputDirective,
  LoadStateComponent,
  MessageBannerComponent,
  PageLayoutComponent,
  RowComponent,
  RowListComponent,
  SelectComponent,
  SettingsSectionComponent,
} from '@gradient/ui/ui';
import { LabelHelpComponent } from '@shared/ui';
import { WritableDirective, ManagedDisableDirective } from '@shared/access';
import { AccessState, ProjectResponse } from '@core/models';

@Component({
  selector: 'app-project-settings',
  standalone: true,
  imports: [
    LabelHelpComponent,
    CommonModule,
    RouterModule,
    FormsModule,
    DialogComponent,
    ButtonComponent,
    InputDirective,
    CopyFieldComponent,
    CheckboxComponent,
    LoadStateComponent,
    WritableDirective,
    ManagedDisableDirective,
    PageLayoutComponent,
    FormFieldComponent,
    SettingsSectionComponent,
    MessageBannerComponent,
    RowComponent,
    SelectComponent,
    RowListComponent,
  ],
  templateUrl: './project-settings.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './project-settings.component.scss',
})
export class ProjectSettingsComponent implements OnInit {
  readonly visibilityOptions = [
    { label: 'Private', value: false },
    { label: 'Public', value: true },
  ];

  private route = inject(ActivatedRoute);
  private router = inject(Router);
  private projectsService = inject(ProjectsService);
  private projectAccess = inject(ProjectAccessService);
  private crumbs = inject(BreadcrumbsService);
  private notify = inject(NotifyService);
  protected config = inject(ConfigService);

  access = signal<AccessState>({ managed: false, canEdit: false, canTrigger: false });

  loading = signal(true);
  loadError = signal<unknown>(null);
  saving = signal(false);
  deleting = signal(false);
  sshLoading = signal(true);
  sshError = signal<unknown>(null);
  generatingSSH = signal(false);

  project = signal<ProjectResponse | null>(null);
  sshKey = signal<string>('');

  showDeleteDialog = signal(false);
  deleteError = signal<string | null>(null);
  showRegenerateKeyDialog = signal(false);
  regenerateError = signal<string | null>(null);
  saveError = signal<string | null>(null);
  saveSuccess = signal(false);

  projectName = '';
  breadcrumb = computed(() => this.crumbs.project(this.projectName, { label: 'Settings' }));

  formData = {
    display_name: '',
    description: '',
    public: false,
    hide_build_requests: false,
    allow_exec_in_build_requests: false,
  };

  ngOnInit(): void {
    this.projectName = this.route.snapshot.paramMap.get('project') || '';
    this.projectAccess.forProject(this.projectName).then((s) => this.access.set(s));
    this.loadProject();
    this.loadSSHKey();
  }

  loadProject(): void {
    this.loading.set(true);
    this.loadError.set(null);
    this.projectsService.getProject(this.projectName).subscribe({
      next: (project) => {
        this.project.set(project);
        this.crumbs.rememberProject(this.projectName, project.display_name);
        this.formData = {
          display_name: project.display_name,
          description: project.description,
          public: project.public,
          hide_build_requests: project.hide_build_requests,
          allow_exec_in_build_requests: project.allow_exec_in_build_requests,
        };
        this.loading.set(false);
      },
      error: (error: unknown) => {
        this.loadError.set(error);
        this.loading.set(false);
      },
    });
  }

  loadSSHKey(): void {
    this.sshLoading.set(true);
    this.sshError.set(null);
    this.projectsService.getSSHKey(this.projectName).subscribe({
      next: (key) => {
        this.sshKey.set(key);
        this.sshLoading.set(false);
      },
      error: (error) => {
        this.sshError.set(error);
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
      allow_exec_in_build_requests: this.formData.allow_exec_in_build_requests,
    }).subscribe({
      next: () => {
        visibilityCall.subscribe({
          next: () => {
            this.saving.set(false);
            this.saveSuccess.set(true);
            this.loadProject();
          },
          error: (error: unknown) => {
            this.saveError.set(apiErrorMessage(error, 'Failed to update visibility.'));
            this.saving.set(false);
            this.loadProject();
          },
        });
      },
      error: (error: unknown) => {
        this.saveError.set(apiErrorMessage(error, 'Failed to save settings.'));
        this.saving.set(false);
      },
    });
  }

  openDeleteDialog(): void {
    this.deleteError.set(null);
    this.showDeleteDialog.set(true);
  }

  deleteProject(): void {
    this.deleting.set(true);
    this.deleteError.set(null);
    this.projectsService.deleteProject(this.projectName).subscribe({
      next: () => {
        this.notify.success('Project deleted');
        this.router.navigate(['/projects']);
      },
      error: (error: unknown) => {
        this.deleteError.set(apiErrorMessage(error, 'Project deletion failed.'));
        this.deleting.set(false);
      },
    });
  }

  openRegenerateKeyDialog(): void {
    this.regenerateError.set(null);
    this.showRegenerateKeyDialog.set(true);
  }

  confirmRegenerateSSHKey(): void {
    this.regenerateError.set(null);
    this.requestSSHKey().subscribe({
      next: () => this.showRegenerateKeyDialog.set(false),
      error: (error: unknown) => this.regenerateError.set(apiErrorMessage(error, 'SSH key generation failed.')),
    });
  }

  generateSSHKey(): void {
    this.requestSSHKey().subscribe({
      error: (error: unknown) => this.notify.failure('SSH key generation failed', error),
    });
  }

  private requestSSHKey(): Observable<string> {
    this.generatingSSH.set(true);
    return this.projectsService.generateSSHKey(this.projectName).pipe(
      tap((key) => {
        this.sshKey.set(key);
        this.notify.success('SSH key generated');
      }),
      finalize(() => this.generatingSSH.set(false)),
    );
  }
}
