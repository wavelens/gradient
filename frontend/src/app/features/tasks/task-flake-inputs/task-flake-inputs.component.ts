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
import { ConfirmService } from '@shared/confirm';
import { LabelHelpComponent } from '@shared/ui';
import { fieldChecks } from '@shared/forms';
import { WritableDirective, ManagedDisableDirective, ServerConfigurationManagedBannerComponent, requestIfProjectMember } from '@shared/access';
import { injectTaskAccess, injectTaskAccessData } from '@core/resolvers/inject-access';
import { FlakeInputOverrideOut, CreateFlakeInputOverrideRequest } from '@core/models';

interface FlakeInputFormState {
  input_name: string;
  url: string;
  keepUrl: boolean;
}

const INPUT_NAME_CHARS = /^[A-Za-z0-9_\-*?[\]]+$/;
const INPUT_NAME_START = /^[A-Za-z_]/;
const GLOB = /[*?[]/;
const pinnedNixFlakeUrlRegExp = /[?&](?:rev|narHash)=|^path:\/nix\/store\/|^(?:github|gitlab|sourcehut):[^/?]+\/[^/?]+\/[0-9a-f]{40}(?:[?#]|$)/;

const DEFAULT_FORM: FlakeInputFormState = {
  input_name: '',
  url: '',
  keepUrl: false,
};

@Component({
  selector: 'app-task-flake-inputs',
  standalone: true,
  imports: [
    ServerConfigurationManagedBannerComponent,
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
  private notify = inject(NotifyService);
  private confirm = inject(ConfirmService);

  access = injectTaskAccess();
  private taskAccessData = injectTaskAccessData();


  loading = signal(true);
  loadError = signal<unknown>(null);
  saving = signal(false);

  projectName = '';
  taskName = '';
  breadcrumb = computed(() => this.crumbs.taskSettings(this.projectName, this.taskName, { label: 'Flake Inputs' }));

  overrides = signal<FlakeInputOverrideOut[]>([]);
  editingId = signal<string | null>(null);
  showCreateDialog = signal(false);
  showEditDialog = signal(false);
  error = signal<string | null>(null);

  form: FlakeInputFormState = { ...DEFAULT_FORM };
  submitAttempted = signal(false);
  private loadedForm = '';

  get inputNameError(): string | null {
    const name = this.form.input_name.trim();
    if (!name) return 'Enter an input name.';
    if (!INPUT_NAME_CHARS.test(name)) return 'Use letters, digits, _, - and the glob characters *, ?, [, ].';
    if (!GLOB.test(name) && !INPUT_NAME_START.test(name)) return 'Start a name without glob characters with a letter or _.';
    const taken = this.overrides().some((o) => o.id !== this.editingId() && o.input_name === name);
    return taken ? 'This task already has an override for this input.' : null;
  }

  get urlError(): string | null {
    if (this.form.keepUrl) return null;
    if (!this.form.url.trim()) return 'Enter a flake URL, or turn on the option above to use the URL from flake.nix.';
    return fieldChecks.remoteFetchableFlakeUrl(this.form.url);
  }

  get unpinnedUrlWarning(): string | null {
    const url = this.form.url.trim();
    if (this.form.keepUrl || !url || this.urlError || pinnedNixFlakeUrlRegExp.test(url)) return null;
    return "The URL doesn't name a commit, so the next evaluation cannot run. Add a commit (e.g. github:NixOS/nixpkgs/<commit> or ?rev=<commit>).";
  }

  submitForm(): void {
    if (this.editingId()) this.saveEdit();
    else this.saveCreate();
  }

  private requestBody(): CreateFlakeInputOverrideRequest | null {
    if (this.saving()) return null;
    this.submitAttempted.set(true);
    if (this.inputNameError || this.urlError) return null;
    return { input_name: this.form.input_name.trim(), url: this.form.keepUrl ? null : this.form.url.trim() };
  }

  ngOnInit(): void {
    this.projectName = this.route.snapshot.paramMap.get('project') || '';
    this.taskName = this.route.snapshot.paramMap.get('task') || '';
    this.loadOverrides();
  }

  loadOverrides(): void {
    this.loading.set(true);
    this.loadError.set(null);
    requestIfProjectMember(this.taskAccessData()?.task, () => this.overridesService.list(this.projectName, this.taskName)).subscribe({
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
    this.editingId.set(null);
    this.error.set(null);
    this.submitAttempted.set(false);
    this.showCreateDialog.set(true);
  }

  startEdit(override: FlakeInputOverrideOut): void {
    this.form = {
      input_name: override.input_name,
      url: override.url ?? '',
      keepUrl: override.url === null,
    };
    this.loadedForm = JSON.stringify(this.form);
    this.editingId.set(override.id);
    this.error.set(null);
    this.submitAttempted.set(false);
    this.showEditDialog.set(true);
  }

  cancelEdit(): void {
    this.showCreateDialog.set(false);
    this.showEditDialog.set(false);
    this.editingId.set(null);
    this.error.set(null);
  }

  saveCreate(): void {
    const body = this.requestBody();
    if (!body) return;
    this.saving.set(true);
    this.error.set(null);
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
    const body = this.requestBody();
    if (!body) return;
    if (JSON.stringify(this.form) === this.loadedForm) {
      this.cancelEdit();
      return;
    }
    this.saving.set(true);
    this.error.set(null);
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

  deleteOverride(override: FlakeInputOverrideOut): void {
    this.confirm.confirm({
      title: 'Delete Override',
      target: override.input_name,
      consequence: 'Later evaluations use this input again as flake.lock pinned it.',
      isIrreversible: true,
      confirmLabel: 'Delete',
      failure: 'Failed to delete the override.',
      action: () => this.overridesService.delete(this.projectName, this.taskName, override.id),
    }).subscribe(() => {
      this.notify.success('Override deleted');
      this.loadOverrides();
    });
  }

  modeLabel(override: FlakeInputOverrideOut): string {
    return override.url === null ? 'Force update' : 'URL override';
  }

  isPattern(override: FlakeInputOverrideOut): boolean {
    return GLOB.test(override.input_name);
  }
}
