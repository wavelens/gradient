/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, computed, inject, signal, ChangeDetectionStrategy } from '@angular/core';
import { CommonModule } from '@angular/common';
import { ActivatedRoute, Router, RouterModule } from '@angular/router';
import { FormsModule } from '@angular/forms';
import { Observable, concat, finalize, switchMap, toArray } from 'rxjs';
import { BreadcrumbsService } from '@core/services/breadcrumbs.service';
import { ConfigService } from '@core/services/config.service';
import { ProjectsService } from '@core/services/projects.service';
import { NotifyService } from '@core/services/notify.service';
import { apiErrorMessage } from '@core/services/api.service';
import {
  BadgeComponent,
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
import { LabelHelpComponent, formatEntityLabel } from '@shared/ui';
import { RelativeTimePipe, ServerTimestampToLocalDateTimePipe } from '@shared/text';
import { AccessService, WritableDirective, ManagedDisableDirective, ServerConfigurationManagedBannerComponent, requestIfProjectMember, isViewerProjectMember, injectSaveSettingsWithPrivateToPublicConfirmation } from '@shared/access';
import { ConfirmService } from '@shared/confirm';
import { maxDisplayNameCharacterCount, splitLines, unsavedChanges } from '@shared/forms';
import { CanComponentDeactivate } from '@core/guards/unsaved-changes.guard';
import { GitSshHostKeyResponse, PatchProjectRequest, ProjectResponse, accessTo } from '@core/models';
import { injectProject } from '@core/resolvers/project-access.resolver';
import { links } from '@shared/routing';

interface ProjectForm {
  display_name: string;
  description: string;
  public: boolean;
  hide_build_requests: boolean;
  allow_exec_in_build_requests: boolean;
  restrict_evaluation_fetches: boolean;
  extra_allowed_uri_prefixes: string;
}

@Component({
  selector: 'app-project-settings',
  standalone: true,
  imports: [
    LabelHelpComponent,
    RelativeTimePipe,
    ServerTimestampToLocalDateTimePipe,
    BadgeComponent,
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
  protected readonly maxDisplayNameCharacterCount = maxDisplayNameCharacterCount;
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
  private saveSettingsWithPrivateToPublicConfirmation = injectSaveSettingsWithPrivateToPublicConfirmation();
  protected accessService = inject(AccessService);
  protected config = inject(ConfigService);

  access = computed(() => accessTo(this.resolvedProject(), 'manageProjectSettings'));
  sshKeyAccess = computed(() => accessTo(this.resolvedProject(), 'manageSshKey'));
  deleteAccess = computed(() => accessTo(this.resolvedProject(), 'deleteProject'));
  webhooksAccess = computed(() => accessTo(this.resolvedProject(), 'manageWebhooks'));
  isViewerProjectMember = computed(() => isViewerProjectMember(this.resolvedProject()));

  loading = signal(true);
  loadError = signal<unknown>(null);
  saving = signal(false);
  sshLoading = signal(true);
  sshError = signal<unknown>(null);
  generatingSSH = signal(false);
  sshHostKeysLoading = signal(true);
  gitSshHostKeysLoadError = signal<unknown>(null);
  hostKeyIdBeingScanned = signal<string | null>(null);

  project = signal<ProjectResponse | null>(null);
  sshKey = signal<string>('');
  sshHostKeys = signal<GitSshHostKeyResponse[]>([]);

  saveError = signal<string | null>(null);

  projectName = '';
  breadcrumb = computed(() => this.crumbs.project(this.projectName, { label: 'Settings' }));

  formData: ProjectForm = {
    display_name: '',
    description: '',
    public: false,
    hide_build_requests: false,
    allow_exec_in_build_requests: false,
    restrict_evaluation_fetches: true,
    extra_allowed_uri_prefixes: '',
  };
  private savedForm: ProjectForm = { ...this.formData };
  readonly unsaved = unsavedChanges(() => this.formData, 'Project Settings');

  get displayNameError(): string | null {
    return this.formData.display_name.trim() ? null : 'Enter a display name.';
  }

  get extraAllowedUriPrefixesError(): string | null {
    return splitLines(this.formData.extra_allowed_uri_prefixes).some((prefix) => /\s/.test(prefix))
      ? 'Remove spaces in each prefix.'
      : null;
  }

  get saveBlockedReason(): string | null {
    return this.displayNameError || this.extraAllowedUriPrefixesError ? 'Fix the marked fields to save.' : null;
  }

  canDeactivate(): boolean | Observable<boolean> {
    return this.unsaved.canLeave();
  }

  ngOnInit(): void {
    this.projectName = this.route.snapshot.paramMap.get('project') || '';
    this.loadProject();
    this.loadSSHKey();
    this.loadSSHHostKeys();
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
          restrict_evaluation_fetches: project.restrict_evaluation_fetches,
          extra_allowed_uri_prefixes: project.extra_allowed_uri_prefixes.join('\n'),
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

  loadSSHHostKeys(): void {
    this.sshHostKeysLoading.set(true);
    this.gitSshHostKeysLoadError.set(null);
    requestIfProjectMember(this.resolvedProject(), () => this.projectsService.listSSHHostKeys(this.projectName)).subscribe({
      next: (keys) => {
        this.sshHostKeys.set(keys);
        this.sshHostKeysLoading.set(false);
      },
      error: (error) => {
        this.gitSshHostKeysLoadError.set(error);
        this.sshHostKeysLoading.set(false);
      },
    });
  }

  saveSettings(): void {
    if (this.saving() || !this.accessService.isWritable(this.access()) || this.saveBlockedReason) return;
    const saved = this.savedForm;
    const { public: visibility, extra_allowed_uri_prefixes, ...fields } = this.formData;
    const patch: PatchProjectRequest = Object.fromEntries(
      Object.entries(fields).filter(([key, value]) => saved[key as keyof ProjectForm] !== value),
    );
    if (extra_allowed_uri_prefixes !== saved.extra_allowed_uri_prefixes) {
      patch.extra_allowed_uri_prefixes = splitLines(extra_allowed_uri_prefixes);
    }
    const save = () => {
      this.saving.set(true);
      const requests: Observable<unknown>[] = [];
      if (Object.keys(patch).length > 0) {
        requests.push(this.projectsService.updateProject(this.projectName, patch));
      }
      if (visibility !== saved.public) {
        requests.push(visibility ? this.projectsService.setPublic(this.projectName) : this.projectsService.setPrivate(this.projectName));
      }
      return concat(...requests).pipe(toArray());
    };

    this.saveError.set(null);
    this.saveSettingsWithPrivateToPublicConfirmation({
      kind: 'project',
      target: formatEntityLabel({ display_name: this.project()?.display_name, name: this.projectName }),
      becomesPublic: visibility && !saved.public,
      save,
    })
      .pipe(finalize(() => this.saving.set(false)))
      .subscribe({
        next: () => {
          this.notify.success('Project settings saved');
          this.loadProject();
        },
        error: (error: unknown) => this.saveError.set(apiErrorMessage(error, 'Failed to save settings.')),
      });
  }

  deleteProject(): void {
    this.confirm
      .confirm({
        title: 'Delete project',
        target: formatEntityLabel({ display_name: this.project()?.display_name, name: this.projectName }),
        consequence:
          "The project's tasks, evaluations, builds, logs, secrets, workers and members are deleted.",
        isIrreversible: true,
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
        target: formatEntityLabel({ display_name: this.project()?.display_name, name: this.projectName }),
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

  scanHostConfirmFingerprintAndSave(key: GitSshHostKeyResponse): void {
    this.hostKeyIdBeingScanned.set(key.id);
    this.projectsService
      .scanSavedHostKey(this.projectName, key.id)
      .pipe(
        finalize(() => this.hostKeyIdBeingScanned.set(null)),
        switchMap((scanned) =>
          this.confirm.confirm({
            title: 'Trust new host key',
            target: `${key.host}:${key.port}`,
            consequence: `The Git server's current key, with fingerprint ${scanned.fingerprint}. Compare the fingerprint with the value that the Git host publishes. Fetches for this project trust only this key from now on.`,
            confirmLabel: 'Trust',
            failure: 'Failed to save the host key.',
            action: () =>
              this.projectsService.saveHostKey(this.projectName, {
                host: key.host,
                port: key.port,
                public_key: scanned.public_key,
              }),
          }),
        ),
      )
      .subscribe({
        next: () => {
          this.notify.success('Host key saved');
          this.loadSSHHostKeys();
        },
        error: (error: unknown) => this.notify.failure('Reading the host key from the Git server failed', error),
      });
  }

  confirmAndDeleteSavedHostKey(key: GitSshHostKeyResponse): void {
    this.confirm
      .confirm({
        title: 'Delete Host Key',
        target: `${key.host}:${key.port}`,
        consequence: 'The next SSH fetch from this Git server can trust the key the server presents and save it again.',
        confirmLabel: 'Delete',
        failure: 'Failed to delete host key.',
        action: () => this.projectsService.deleteSavedHostKey(this.projectName, key.id),
      })
      .subscribe(() => {
        this.notify.success('Host key deleted');
        this.loadSSHHostKeys();
      });
  }

  private showSSHKey(key: string): void {
    this.sshKey.set(key);
    this.notify.success('SSH key generated');
  }
}
