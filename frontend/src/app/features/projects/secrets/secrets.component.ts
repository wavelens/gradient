/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, OnInit, computed, inject, signal } from '@angular/core';
import { FormsModule } from '@angular/forms';
import { ActivatedRoute } from '@angular/router';
import { BreadcrumbsService } from '@core/services/breadcrumbs.service';
import { ProjectAccessService } from '@core/services/project-access.service';
import { ProjectSecretsService } from '@core/services/project-secrets.service';
import { AccessState, ProjectSecretSummary } from '@core/models';
import {
  BadgeComponent,
  ButtonComponent,
  DialogComponent,
  EmptyStateComponent,
  FormFieldComponent,
  InputDirective,
  LoadingSpinnerComponent,
  MessageBannerComponent,
  PageLayoutComponent,
  RowComponent,
  RowListComponent,
} from '@gradient/ui/ui';
import { WritableDirective } from '@shared/access';
import { relativeTime } from '@shared/text';

const SECRET_NAME = /^[a-z0-9][a-z0-9-]{0,63}$/;

@Component({
  selector: 'app-project-secrets',
  standalone: true,
  imports: [
    FormsModule,
    BadgeComponent,
    ButtonComponent,
    DialogComponent,
    EmptyStateComponent,
    FormFieldComponent,
    InputDirective,
    LoadingSpinnerComponent,
    MessageBannerComponent,
    PageLayoutComponent,
    RowComponent,
    RowListComponent,
    WritableDirective,
  ],
  templateUrl: './secrets.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './secrets.component.scss',
})
export class SecretsComponent implements OnInit {
  private route = inject(ActivatedRoute);
  private secretsService = inject(ProjectSecretsService);
  private projectAccess = inject(ProjectAccessService);
  private crumbs = inject(BreadcrumbsService);

  protected readonly relativeTime = relativeTime;

  access = signal<AccessState>({ managed: false, canEdit: false, canTrigger: false });
  loading = signal(true);
  secrets = signal<ProjectSecretSummary[]>([]);
  pageError = signal<string | null>(null);

  formOpen = signal(false);
  replacing = signal(false);
  name = signal('');
  value = signal('');
  saving = signal(false);
  formError = signal<string | null>(null);

  deleteName = signal<string | null>(null);
  deleting = signal(false);

  projectName = '';
  breadcrumb = computed(() => this.crumbs.projectSettings(this.projectName, { label: 'Secrets' }));
  nameInvalid = computed(() => this.name() !== '' && !SECRET_NAME.test(this.name()));
  canSave = computed(() => SECRET_NAME.test(this.name()) && this.value() !== '' && !this.saving());

  ngOnInit(): void {
    this.projectName = this.route.snapshot.paramMap.get('project') || '';
    this.projectAccess.forProject(this.projectName).then((state) => this.access.set(state));
    this.loadSecrets();
  }

  loadSecrets(): void {
    this.loading.set(true);
    this.secretsService.list(this.projectName).subscribe({
      next: (secrets) => {
        this.secrets.set(secrets);
        this.loading.set(false);
      },
      error: (error) => {
        this.pageError.set(error?.message || 'Failed to load secrets.');
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
    this.saving.set(true);
    this.formError.set(null);
    this.secretsService.put(this.projectName, this.name(), this.value()).subscribe({
      next: () => {
        this.saving.set(false);
        this.closeForm();
        this.loadSecrets();
      },
      error: (error) => {
        this.formError.set(error?.message || 'Failed to save the secret.');
        this.saving.set(false);
      },
    });
  }

  closeForm(): void {
    this.formOpen.set(false);
    this.value.set('');
  }

  confirmDelete(): void {
    const name = this.deleteName();
    if (!name) return;
    this.deleting.set(true);
    this.pageError.set(null);
    this.secretsService.remove(this.projectName, name).subscribe({
      next: () => {
        this.deleting.set(false);
        this.deleteName.set(null);
        this.loadSecrets();
      },
      error: (error) => {
        this.pageError.set(error?.message || 'Failed to delete the secret.');
        this.deleting.set(false);
        this.deleteName.set(null);
      },
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
