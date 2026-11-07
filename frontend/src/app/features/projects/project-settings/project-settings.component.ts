/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, computed, inject, signal, ChangeDetectionStrategy } from '@angular/core';
import { CommonModule } from '@angular/common';
import { ActivatedRoute, Router, RouterModule } from '@angular/router';
import { FormsModule } from '@angular/forms';
import { finalize } from 'rxjs';
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
import { ConfirmService } from '@shared/confirm';
import { AccessState, ProjectResponse } from '@core/models';

@Component({
  selector: 'app-project-settings',
  standalone: true,
  imports: [
    LabelHelpComponent,
    CommonModule,
    RouterModule,
    FormsModule,
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
  private confirm = inject(ConfirmService);
  protected config = inject(ConfigService);

  access = signal<AccessState>({ managed: false, canEdit: false, canTrigger: false });

  loading = signal(true);
  loadError = signal<unknown>(null);
  saving = signal(false);
  sshLoading = signal(true);
  sshError = signal<unknown>(null);
  generatingSSH = signal(false);

  project = signal<ProjectResponse | null>(null);
  sshKey = signal<string>('');

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

  deleteProject(): void {
    this.confirm
      .confirm({
        title: 'Delete project',
        target: this.project()?.display_name ?? this.projectName,
        consequence: "This action can't be undone.",
        confirmLabel: 'Delete',
        failure: 'Project deletion failed.',
        action: () => this.projectsService.deleteProject(this.projectName),
      })
      .subscribe(() => {
        this.notify.success('Project deleted');
        this.router.navigate(['/projects']);
      });
  }

  regenerateSSHKey(): void {
    this.confirm
      .confirm({
        title: 'Regenerate SSH key',
        target: this.project()?.display_name ?? this.projectName,
        consequence: 'The existing key will stop working immediately and any repositories using it will lose access.',
        confirmLabel: 'Regenerate',
        failure: 'SSH key generation failed.',
        action: () => this.projectsService.generateSSHKey(this.projectName),
      })
      .subscribe((key) => this.showSSHKey(key));
  }

  generateSSHKey(): void {
    this.generatingSSH.set(true);
    this.projectsService
      .generateSSHKey(this.projectName)
      .pipe(finalize(() => this.generatingSSH.set(false)))
      .subscribe({
        next: (key) => this.showSSHKey(key),
        error: (error: unknown) => this.notify.failure('SSH key generation failed', error),
      });
  }

  private showSSHKey(key: string): void {
    this.sshKey.set(key);
    this.notify.success('SSH key generated');
  }
}
