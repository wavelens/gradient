/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, OnInit, computed, inject, signal } from '@angular/core';
import { FormsModule } from '@angular/forms';
import { ActivatedRoute } from '@angular/router';
import { BreadcrumbsService } from '@core/services/breadcrumbs.service';
import { injectProject } from '@core/resolvers/project-access.resolver';
import { ProjectSecretsService } from '@core/services/project-secrets.service';
import { NotifyService } from '@core/services/notify.service';
import { apiErrorMessage } from '@core/services/api.service';
import { accessTo, ProjectSecretSummary, SopsRecipientResponse } from '@core/models';
import { docsUrl } from '@core/docs';
import {
  BadgeComponent,
  ButtonComponent,
  CopyFieldComponent,
  DialogComponent,
  FormFieldComponent,
  InputDirective,
  LoadStateComponent,
  PageLayoutComponent,
  RowComponent,
  RowListComponent,
  SettingsSectionComponent,
} from '@gradient/ui/ui';
import { WritableDirective, createMissingPermissionLoadStateError } from '@shared/access';
import { ConfirmService } from '@shared/confirm';
import { LabelHelpComponent } from '@shared/ui';
import { relativeTime } from '@shared/text';

const SECRET_NAME = /^[a-z0-9][a-z0-9-]{0,63}$/;

@Component({
  selector: 'app-project-secrets',
  standalone: true,
  imports: [
    FormsModule,
    BadgeComponent,
    ButtonComponent,
    CopyFieldComponent,
    DialogComponent,
    FormFieldComponent,
    InputDirective,
    LabelHelpComponent,
    LoadStateComponent,
    PageLayoutComponent,
    RowComponent,
    RowListComponent,
    SettingsSectionComponent,
    WritableDirective,
  ],
  templateUrl: './secrets.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './secrets.component.scss',
})
export class SecretsComponent implements OnInit {
  private route = inject(ActivatedRoute);
  private secretsService = inject(ProjectSecretsService);
  private resolvedProject = injectProject();
  private crumbs = inject(BreadcrumbsService);
  private notify = inject(NotifyService);
  private confirm = inject(ConfirmService);

  protected readonly relativeTime = relativeTime;
  protected readonly docsUrl = docsUrl('exec/secrets/');

  access = computed(() => accessTo(this.resolvedProject(), 'manageProjectSettings'));
  missingPermission = createMissingPermissionLoadStateError('manageProjectSettings', 'project');
  loading = signal(true);
  loadError = signal<unknown>(null);
  secrets = signal<ProjectSecretSummary[]>([]);
  sopsRecipient = signal<SopsRecipientResponse | null>(null);

  formOpen = signal(false);
  replacing = signal(false);
  name = signal('');
  value = signal('');
  saving = signal(false);
  formError = signal<string | null>(null);

  projectName = '';
  breadcrumb = computed(() => this.crumbs.projectSettings(this.projectName, { label: 'Secrets' }));
  nameInvalid = computed(() => this.name() !== '' && !SECRET_NAME.test(this.name()));
  canSave = computed(() => SECRET_NAME.test(this.name()) && this.value() !== '' && !this.saving());

  ngOnInit(): void {
    this.projectName = this.route.snapshot.paramMap.get('project') || '';
    this.loadSecrets();
    this.loadSopsRecipient();
  }

  loadSopsRecipient(): void {
    this.secretsService.sopsRecipient(this.projectName).subscribe({
      next: (recipient) => this.sopsRecipient.set(recipient),
      error: () => this.sopsRecipient.set(null),
    });
  }

  loadSecrets(): void {
    if (!this.access().canEdit) {
      this.loading.set(false);
      return;
    }
    this.loading.set(true);
    this.loadError.set(null);
    this.secretsService.list(this.projectName).subscribe({
      next: (secrets) => {
        this.secrets.set(secrets);
        this.loading.set(false);
      },
      error: (error: unknown) => {
        this.loadError.set(error);
        this.loading.set(false);
      },
    });
  }

  startCreate(): void {
    this.openForm('', false);
  }

  startReplace(secret: ProjectSecretSummary): void {
    this.openForm(secret.name, true);
  }

  save(): void {
    if (!this.canSave()) return;
    const name = this.name();
    const value = this.value();
    if (!this.replacing() && this.secrets().some((secret) => secret.name === name)) {
      this.confirm
        .confirm({
          title: 'Replace Secret',
          target: name,
          consequence: 'A secret with this name already exists. Replacements overwrite the current value with no restore.',
          confirmLabel: 'Replace',
          failure: 'Failed to save the secret.',
          action: () => this.secretsService.put(this.projectName, name, value),
        })
        .subscribe(() => this.saved());
      return;
    }
    this.saving.set(true);
    this.formError.set(null);
    this.secretsService.put(this.projectName, name, value).subscribe({
      next: () => {
        this.saving.set(false);
        this.saved();
      },
      error: (error: unknown) => {
        this.formError.set(apiErrorMessage(error, 'Failed to save the secret.'));
        this.saving.set(false);
      },
    });
  }

  private saved(): void {
    this.closeForm();
    this.notify.success('Secret saved');
    this.loadSecrets();
  }

  closeForm(): void {
    this.formOpen.set(false);
    this.value.set('');
  }

  startDelete(name: string): void {
    this.confirm
      .confirm({
        title: 'Delete secret',
        target: name,
        consequence: 'Exec jobs that list it fail until it is set again.',
        confirmLabel: 'Delete',
        failure: 'Failed to delete the secret.',
        action: () => this.secretsService.remove(this.projectName, name),
      })
      .subscribe(() => {
        this.notify.success('Secret deleted');
        this.loadSecrets();
      });
  }

  private openForm(name: string, replacing: boolean): void {
    this.name.set(name);
    this.value.set('');
    this.replacing.set(replacing);
    this.formError.set(null);
    this.formOpen.set(true);
  }
}
