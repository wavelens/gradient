/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, inject, signal, computed } from '@angular/core';
import { CommonModule } from '@angular/common';
import { ActivatedRoute, RouterModule } from '@angular/router';
import { FormsModule } from '@angular/forms';
import { DialogModule } from 'primeng/dialog';
import { ButtonModule } from 'primeng/button';
import { InputTextModule } from 'primeng/inputtext';
import { CheckboxModule } from 'primeng/checkbox';
import { ToastModule } from 'primeng/toast';
import { MessageService } from 'primeng/api';
import { FlakeInputOverridesService } from '@core/services/flake-input-overrides.service';
import { OrganizationsService } from '@core/services/organizations.service';
import { LoadingSpinnerComponent } from '@shared/components/loading-spinner/loading-spinner.component';
import { WritableDirective, ManagedDisableDirective, AccessService } from '@shared/access';
import { injectProjectAccess } from '@core/resolvers/inject-access';
import { FlakeInputOverride, CreateFlakeInputOverrideBody } from '@core/models';

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
  selector: 'app-project-flake-inputs',
  standalone: true,
  imports: [
    CommonModule,
    RouterModule,
    FormsModule,
    DialogModule,
    ButtonModule,
    InputTextModule,
    CheckboxModule,
    ToastModule,
    LoadingSpinnerComponent,
    WritableDirective,
    ManagedDisableDirective,
  ],
  providers: [MessageService],
  templateUrl: './project-flake-inputs.component.html',
  styleUrl: './project-flake-inputs.component.scss',
})
export class ProjectFlakeInputsComponent implements OnInit {
  private route = inject(ActivatedRoute);
  private overridesService = inject(FlakeInputOverridesService);
  private orgsService = inject(OrganizationsService);
  private messageService = inject(MessageService);
  private accessSvc = inject(AccessService);

  access = injectProjectAccess();

  rowDisabled = computed(
    () =>
      this.deletingId() !== null ||
      this.accessSvc.shouldDisableInput(this.access()),
  );

  loading = signal(true);
  saving = signal(false);
  deletingId = signal<string | null>(null);

  orgName = '';
  orgDisplayName = signal('');
  projectName = '';

  overrides = signal<FlakeInputOverride[]>([]);
  editingId = signal<string | null>(null);
  showCreateDialog = signal(false);
  showEditDialog = signal(false);
  error = signal<string | null>(null);

  form: FlakeInputFormState = { ...DEFAULT_FORM };

  ngOnInit(): void {
    this.orgName = this.route.snapshot.paramMap.get('org') || '';
    this.projectName = this.route.snapshot.paramMap.get('project') || '';
    this.orgsService.getOrganization(this.orgName).subscribe({
      next: (org) => this.orgDisplayName.set(org.display_name),
      error: () => {},
    });
    this.loadOverrides();
  }

  loadOverrides(): void {
    this.loading.set(true);
    this.overridesService.list(this.orgName, this.projectName).subscribe({
      next: (list) => {
        this.overrides.set(list);
        this.loading.set(false);
      },
      error: () => this.loading.set(false),
    });
  }

  startCreate(): void {
    this.form = { ...DEFAULT_FORM };
    this.error.set(null);
    this.showCreateDialog.set(true);
  }

  startEdit(override: FlakeInputOverride): void {
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
    const body: CreateFlakeInputOverrideBody = {
      input_name: this.form.input_name,
      url: this.form.keepUrl ? null : this.form.url,
    };
    this.overridesService.create(this.orgName, this.projectName, body).subscribe({
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
        this.error.set(err?.message || 'Failed to create override.');
        this.saving.set(false);
      },
    });
  }

  saveEdit(): void {
    const id = this.editingId();
    if (!id) return;
    this.saving.set(true);
    this.error.set(null);
    const body: CreateFlakeInputOverrideBody = {
      input_name: this.form.input_name,
      url: this.form.keepUrl ? null : this.form.url,
    };
    this.overridesService.update(this.orgName, this.projectName, id, body).subscribe({
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
        this.error.set(err?.message || 'Failed to update override.');
        this.saving.set(false);
      },
    });
  }

  deleteOverride(id: string): void {
    if (!confirm('Delete this flake input override? This cannot be undone.')) return;
    this.deletingId.set(id);
    this.overridesService.delete(this.orgName, this.projectName, id).subscribe({
      next: () => {
        this.deletingId.set(null);
        this.loadOverrides();
      },
      error: () => this.deletingId.set(null),
    });
  }

  modeLabel(override: FlakeInputOverride): string {
    return override.url === null ? 'Keep URL' : 'URL override';
  }

  isPattern(override: FlakeInputOverride): boolean {
    return /[*?[]/.test(override.input_name);
  }
}
