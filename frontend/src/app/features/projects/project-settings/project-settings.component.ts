/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, computed, inject, signal, ChangeDetectionStrategy } from '@angular/core';
import { CommonModule } from '@angular/common';
import { ActivatedRoute, Router, RouterModule } from '@angular/router';
import { FormsModule } from '@angular/forms';
import { Observable, concat, finalize, throwError, catchError } from 'rxjs';
import { BreadcrumbsService } from '@core/services/breadcrumbs.service';
import { ConfigService } from '@core/services/config.service';
import { ProjectsService } from '@core/services/projects.service';
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
  SubmitDirective,
} from '@gradient/ui/ui';
import { LabelHelpComponent } from '@shared/ui';
import { AccessService, WritableDirective, ManagedDisableDirective, ServerConfigurationManagedBannerComponent, requestIfProjectMember } from '@shared/access';
import { ConfirmService } from '@shared/confirm';
import { unsavedChanges } from '@shared/forms';
import { CanComponentDeactivate } from '@core/guards/unsaved-changes.guard';
import { ProjectResponse, accessTo } from '@core/models';
import { injectProject } from '@core/resolvers/project-access.resolver';
import { links } from '@shared/routing';

interface ProjectForm {
  display_name: string;
  description: string;
  public: boolean;
  hide_build_requests: boolean;
  allow_exec_in_build_requests: boolean;
}

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
    ManagedDisableDirective, ServerConfigurationManagedBannerComponent,
    PageLayoutComponent,
    FormFieldComponent,
    SettingsSectionComponent,
    MessageBannerComponent,
    RowComponent,
    SelectComponent,
    RowListComponent,
    SubmitDirective,
  ],
  templateUrl: './project-settings.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './project-settings.component.scss',
})
export class ProjectSettingsComponent implements OnInit, CanComponentDeactivate {
  protected readonly links = links;
  readonly visibilityOptions = [
    { label: 'Private', value: false },
    { label: 'Public', value: true },
  ];

  private route = inject(ActivatedRoute);
  private router = inject(Router);
  private projectsService = inject(ProjectsService);
  private resolvedProject = injectProject();
  private crumbs = inject(BreadcrumbsService);
  private notify = inject(NotifyService);
  private confirm = inject(ConfirmService);
  protected accessService = inject(AccessService);
  protected config = inject(ConfigService);

  access = computed(() => accessTo(this.resolvedProject(), 'manageProjectSettings'));
  sshKeyAccess = computed(() => accessTo(this.resolvedProject(), 'manageProjectSettings', 'manageSshKey'));
  deleteAccess = computed(() => accessTo(this.resolvedProject(), 'deleteProject'));
  manageIntegrationsAccess = computed(() => accessTo(this.resolvedProject(), 'manageIntegrations'));
  webhooksAccess = computed(() => accessTo(this.resolvedProject(), 'manageWebhooks'));

  loading = signal(true);
  loadError = signal<unknown>(null);
  saving = signal(false);
  sshLoading = signal(true);
  sshError = signal<unknown>(null);
  generatingSSH = signal(false);

  project = signal<ProjectResponse | null>(null);
  sshKey = signal<string>('');

  saveError = signal<string | null>(null);

  projectName = '';
  breadcrumb = computed(() => this.crumbs.project(this.projectName, { label: 'Settings' }));

  formData: ProjectForm = {
    display_name: '',
    description: '',
    public: false,
    hide_build_requests: false,
    allow_exec_in_build_requests: false,
  };
  private savedForm: ProjectForm = { ...this.formData };
  readonly unsaved = unsavedChanges(() => this.formData, 'Project Settings');

  get displayNameError(): string | null {
    return this.formData.display_name.trim() ? null : 'Enter a display name.';
  }

  get saveBlockedReason(): string | null {
    return this.displayNameError ? 'Fix the marked fields to save.' : null;
  }

  canDeactivate(): boolean | Observable<boolean> {
    return this.unsaved.canLeave();
  }

  ngOnInit(): void {
    this.projectName = this.route.snapshot.paramMap.get('project') || '';
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
        this.savedForm = { ...this.formData };
        this.unsaved.mark();
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
    requestIfProjectMember(this.resolvedProject(), () => this.projectsService.getSSHKey(this.projectName)).subscribe({
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
    if (this.saving() || !this.accessService.isWritable(this.access()) || this.saveBlockedReason) return;
    const saved = this.savedForm;
    const { public: visibility, ...fields } = this.formData;
    const patch = Object.fromEntries(
      Object.entries(fields).filter(([key, value]) => saved[key as keyof ProjectForm] !== value),
    );
    const requests: Observable<unknown>[] = [];
    if (Object.keys(patch).length > 0) {
      requests.push(this.failWith(this.projectsService.updateProject(this.projectName, patch), 'Failed to save settings.'));
    }
    if (visibility !== saved.public) {
      const visibilityCall = visibility
        ? this.projectsService.setPublic(this.projectName)
        : this.projectsService.setPrivate(this.projectName);
      requests.push(this.failWith(visibilityCall, 'Failed to update visibility.'));
    }

    this.saving.set(true);
    this.saveError.set(null);
    concat(...requests).subscribe({
      complete: () => {
        this.saving.set(false);
        this.notify.success('Project settings saved');
        this.loadProject();
      },
      error: (message: string) => {
        this.saveError.set(message);
        this.saving.set(false);
      },
    });
  }

  private failWith<T>(request: Observable<T>, message: string): Observable<T> {
    return request.pipe(catchError((error: unknown) => throwError(() => apiErrorMessage(error, message))));
  }

  deleteProject(): void {
    this.confirm
      .confirm({
        title: 'Delete project',
        target: this.project()?.display_name ?? this.projectName,
        consequence:
          "Delete all tasks in the project along with their evaluations, builds, and logs. Delete the project's secrets, workers, and members. Nothing can undo this action.",
        confirmLabel: 'Delete',
        failure: 'Project deletion failed.',
        action: () => this.projectsService.deleteProject(this.projectName),
      })
      .subscribe(() => {
        this.notify.success('Project deleted');
        this.router.navigateByUrl(links.projects());
      });
  }

  regenerateSSHKey(): void {
    this.confirm
      .confirm({
        title: 'Regenerate SSH key',
        target: this.project()?.display_name ?? this.projectName,
        consequence:
          "Three things stop working right away with the current key. A repository with this deploy key can lose access. The project can't decrypt secrets that are encrypted in the repository until someone can encrypt them again with the new key. Servers for remote builds reject Gradient until the new key is present in authorized_keys.",
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
