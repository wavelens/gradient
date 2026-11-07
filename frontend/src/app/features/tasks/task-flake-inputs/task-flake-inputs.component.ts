/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, inject, signal, computed, ChangeDetectionStrategy } from '@angular/core';
import { CommonModule } from '@angular/common';
import { ActivatedRoute, RouterModule } from '@angular/router';
import { FormsModule } from '@angular/forms';
import { FlakeInputOverridesService } from '@core/services/flake-input-overrides.service';
import { BreadcrumbsService } from '@core/services/breadcrumbs.service';
import {
  BadgeComponent,
  ButtonComponent,
  CheckboxComponent,
  DialogComponent,
  FormFieldComponent,
  InputDirective,
  LoadStateComponent,
  MessageService,
  PageLayoutComponent,
} from '@gradient/ui/ui';
import { NotifyService } from '@core/services/notify.service';
import { apiErrorMessage } from '@core/services/api.service';
import { LabelHelpComponent } from '@shared/ui';
import { WritableDirective, ManagedDisableDirective, AccessService } from '@shared/access';
import { injectTaskAccess } from '@core/resolvers/inject-access';
import { FlakeInputOverrideOut, CreateFlakeInputOverrideRequest } from '@core/models';

interface FlakeInputFormState {
  input_name: string;
  url: string;
  keepUrl: boolean;
}

const DEFAULT_FORM: FlakeInputFormState = {
  input_name: '',
  url: '',
  keepUrl: false,
};

@Component({
  selector: 'app-task-flake-inputs',
  standalone: true,
  imports: [
    LabelHelpComponent,
    CommonModule,
    RouterModule,
    FormsModule,
    DialogComponent,
    ButtonComponent,
    InputDirective,
    CheckboxComponent,
    LoadStateComponent,
    WritableDirective,
    ManagedDisableDirective,
    PageLayoutComponent,
    FormFieldComponent,
    BadgeComponent,
  ],
  templateUrl: './task-flake-inputs.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './task-flake-inputs.component.scss',
})
export class TaskFlakeInputsComponent implements OnInit {
  private route = inject(ActivatedRoute);
  private overridesService = inject(FlakeInputOverridesService);
  private crumbs = inject(BreadcrumbsService);
  private messageService = inject(MessageService);
  private accessSvc = inject(AccessService);
  private notify = inject(NotifyService);

  access = injectTaskAccess();

  rowDisabled = computed(
    () =>
      this.deletingId() !== null ||
      this.accessSvc.shouldDisableInput(this.access()),
  );

  loading = signal(true);
  loadError = signal<unknown>(null);
  saving = signal(false);
  deletingId = signal<string | null>(null);

  projectName = '';
  taskName = '';
  breadcrumb = computed(() => this.crumbs.taskSettings(this.projectName, this.taskName, { label: 'Flake Inputs' }));

  overrides = signal<FlakeInputOverrideOut[]>([]);
  editingId = signal<string | null>(null);
  showCreateDialog = signal(false);
  showEditDialog = signal(false);
  error = signal<string | null>(null);

  form: FlakeInputFormState = { ...DEFAULT_FORM };

  ngOnInit(): void {
    this.projectName = this.route.snapshot.paramMap.get('project') || '';
    this.taskName = this.route.snapshot.paramMap.get('task') || '';
    this.loadOverrides();
  }

  loadOverrides(): void {
    this.loading.set(true);
    this.loadError.set(null);
    this.overridesService.list(this.projectName, this.taskName).subscribe({
      next: (list) => {
        this.overrides.set(list);
        this.loading.set(false);
      },
      error: (error: unknown) => {
        this.loadError.set(error);
        this.loading.set(false);
      },
    });
  }

  startCreate(): void {
    this.form = { ...DEFAULT_FORM };
    this.error.set(null);
    this.showCreateDialog.set(true);
  }

  startEdit(override: FlakeInputOverrideOut): void {
    this.form = {
      input_name: override.input_name,
      url: override.url ?? '',
      keepUrl: override.url === null,
    };
    this.editingId.set(override.id);
    this.error.set(null);
    this.showEditDialog.set(true);
  }

  cancelEdit(): void {
    this.showCreateDialog.set(false);
    this.showEditDialog.set(false);
    this.editingId.set(null);
    this.error.set(null);
  }

  saveCreate(): void {
    this.saving.set(true);
    this.error.set(null);
    const body: CreateFlakeInputOverrideRequest = {
      input_name: this.form.input_name,
      url: this.form.keepUrl ? null : this.form.url,
    };
    this.overridesService.create(this.projectName, this.taskName, body).subscribe({
      next: () => {
        this.saving.set(false);
        this.showCreateDialog.set(false);
        this.messageService.add({
          severity: 'success',
          summary: 'Saved',
          detail: 'Will apply to your next evaluation.',
        });
        this.loadOverrides();
      },
      error: (err) => {
        this.error.set(apiErrorMessage(err, 'Failed to create override.'));
        this.saving.set(false);
      },
    });
  }

  saveEdit(): void {
    const id = this.editingId();
    if (!id) return;
    this.saving.set(true);
    this.error.set(null);
    const body: CreateFlakeInputOverrideRequest = {
      input_name: this.form.input_name,
      url: this.form.keepUrl ? null : this.form.url,
    };
    this.overridesService.update(this.projectName, this.taskName, id, body).subscribe({
      next: () => {
        this.saving.set(false);
        this.showEditDialog.set(false);
        this.editingId.set(null);
        this.messageService.add({
          severity: 'success',
          summary: 'Saved',
          detail: 'Will apply to your next evaluation.',
        });
        this.loadOverrides();
      },
      error: (err) => {
        this.error.set(apiErrorMessage(err, 'Failed to update override.'));
        this.saving.set(false);
      },
    });
  }

  deleteOverride(id: string): void {
    if (!confirm('Delete this flake input override? This cannot be undone.')) return;
    this.deletingId.set(id);
    this.overridesService.delete(this.projectName, this.taskName, id).subscribe({
      next: () => {
        this.deletingId.set(null);
        this.notify.success('Deleted the override for the flake input');
        this.loadOverrides();
      },
      error: (error: unknown) => {
        this.deletingId.set(null);
        this.notify.failure('Failed to delete the override for the flake input', error);
      },
    });
  }

  modeLabel(override: FlakeInputOverrideOut): string {
    return override.url === null ? 'Keep URL' : 'URL override';
  }

  isPattern(override: FlakeInputOverrideOut): boolean {
    return /[*?[]/.test(override.input_name);
  }
}
