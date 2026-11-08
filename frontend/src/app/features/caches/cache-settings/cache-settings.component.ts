/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, computed, inject, signal, ChangeDetectionStrategy } from '@angular/core';
import { CommonModule } from '@angular/common';
import { ActivatedRoute, Router, RouterModule } from '@angular/router';
import { FormsModule } from '@angular/forms';
import { of } from 'rxjs';
import { BreadcrumbsService } from '@core/services/breadcrumbs.service';
import { CachesService } from '@core/services/caches.service';
import { NotifyService } from '@core/services/notify.service';
import { apiErrorMessage } from '@core/services/api.service';
import {
  ButtonComponent,
  CheckboxComponent,
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
import { WritableDirective, ManagedDisableDirective, AccessService } from '@shared/access';
import { injectCacheAccess, injectCacheAccessData, injectCacheAccessTo } from '@core/resolvers/inject-access';
import { CanComponentDeactivate } from '@core/guards/unsaved-changes.guard';
import { CacheResponse, PatchCacheRequest } from '@core/models';
import { LabelHelpComponent } from '@shared/ui';
import { ConfirmService } from '@shared/confirm';
import { unsavedChanges } from '@shared/forms';
import { links } from '@shared/routing';

interface CacheForm {
  display_name: string;
  description: string;
  priority: number | null;
  local_priority: number | null;
  max_storage_gb: number | null;
  pull_through: boolean;
  public: boolean;
}

type CheckedField = 'display_name' | 'priority' | 'local_priority' | 'max_storage_gb';

const CHECKED_FIELDS: CheckedField[] = ['display_name', 'priority', 'local_priority', 'max_storage_gb'];
const PATCH_FIELDS = ['display_name', 'description', 'priority', 'local_priority', 'max_storage_gb', 'pull_through'] as const;
const PRIORITY_RANGE = 'Enter an integer from 0 to 255.';

const isBlank = (value: unknown): boolean => value === null || value === undefined || String(value).trim() === '';

function isWholeNumber(value: unknown, min: number, max: number): boolean {
  if (isBlank(value)) return false;
  const number = Number(value);
  return Number.isInteger(number) && number >= min && number <= max;
}

@Component({
  selector: 'app-cache-settings',
  standalone: true,
  imports: [
    CommonModule,
    RouterModule,
    FormsModule,
    ButtonComponent,
    InputDirective,
    LoadStateComponent,
    WritableDirective,
    ManagedDisableDirective,
    PageLayoutComponent,
    FormFieldComponent,
    SettingsSectionComponent,
    SubmitDirective,
    MessageBannerComponent,
    RowComponent,
    SelectComponent,
    RowListComponent,
    CheckboxComponent,
    LabelHelpComponent,
  ],
  templateUrl: './cache-settings.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styles: `
    .save-hint {
      align-self: center;
    }
  `,
})
export class CacheSettingsComponent implements OnInit, CanComponentDeactivate {
  protected readonly links = links;
  private route = inject(ActivatedRoute);
  private accessService = inject(AccessService);
  private router = inject(Router);
  private cachesService = inject(CachesService);
  private crumbs = inject(BreadcrumbsService);
  private notify = inject(NotifyService);
  private confirm = inject(ConfirmService);

  private resolved = injectCacheAccessData();
  access = injectCacheAccess();
  deleteAccess = injectCacheAccessTo('deleteCache');

  loading = signal(true);
  loadError = signal<unknown>(null);
  saving = signal(false);
  toggling = signal(false);

  cache = signal<CacheResponse | null>(null);
  saveError = signal<string | null>(null);
  saveSuccess = signal(false);

  cacheName = '';
  breadcrumb = computed(() => this.crumbs.cache(this.cacheName, { label: 'Settings' }));

  readonly visibilityOptions = [
    { label: 'Private', value: false },
    { label: 'Public', value: true },
  ];

  formData: CacheForm = {
    display_name: '',
    description: '',
    priority: 50,
    local_priority: null,
    max_storage_gb: 0,
    pull_through: true,
    public: false,
  };
  private saved: CacheForm = { ...this.formData };
  readonly unsaved = unsavedChanges(() => this.formData, 'Cache Settings');

  canDeactivate() {
    return this.unsaved.canLeave();
  }

  ngOnInit(): void {
    this.cacheName = this.route.snapshot.paramMap.get('cache') || '';
    const resolved = this.resolved()?.cache;
    if (resolved) this.applyCacheRecord(resolved);
    else this.loadCache();
  }

  loadCache(): void {
    this.loading.set(true);
    this.loadError.set(null);
    this.cachesService.getCache(this.cacheName).subscribe({
      next: (cache) => {
        this.crumbs.rememberCache(this.cacheName, cache.display_name);
        this.applyCacheRecord(cache);
      },
      error: (error: unknown) => {
        this.loadError.set(error);
        this.loading.set(false);
      },
    });
  }

  private applyCacheRecord(cache: CacheResponse): void {
    this.cache.set(cache);
    this.saved = {
      display_name: cache.display_name,
      description: cache.description,
      priority: cache.priority,
      local_priority: cache.local_priority ?? null,
      max_storage_gb: cache.max_storage_gb ?? 0,
      pull_through: cache.pull_through ?? true,
      public: cache.public,
    };
    this.formData = { ...this.saved };
    this.unsaved.mark();
    this.loading.set(false);
  }

  fieldError(field: CheckedField): string | null {
    const value = this.formData[field];
    switch (field) {
      case 'display_name':
        return String(value ?? '').trim() ? null : 'Enter a display name.';
      case 'priority':
        return isWholeNumber(value, 0, 255) ? null : PRIORITY_RANGE;
      case 'local_priority':
        return isBlank(value) || isWholeNumber(value, 0, 255) ? null : PRIORITY_RANGE;
      case 'max_storage_gb':
        return isWholeNumber(value, 0, Number.MAX_SAFE_INTEGER)
          ? null
          : 'Enter 0 for no limit, otherwise an integer of at least 1 in GB.';
    }
  }

  canSave(): boolean {
    return (
      !this.saving() &&
      !this.accessService.shouldDisableInput(this.access()) &&
      this.unsaved.changed() &&
      CHECKED_FIELDS.every((field) => !this.fieldError(field))
    );
  }

  saveSettings(): void {
    if (!this.canSave()) return;
    const values: CacheForm = { ...this.formData, display_name: this.formData.display_name.trim() };
    const patch: PatchCacheRequest = {};
    for (const field of PATCH_FIELDS) {
      if (values[field] !== this.saved[field]) Object.assign(patch, { [field]: values[field] });
    }
    const visibilityChanged = values.public !== this.saved.public;
    this.saving.set(true);
    this.saveError.set(null);
    this.saveSuccess.set(false);

    const finish = () => {
      this.saving.set(false);
      this.saved = values;
      this.formData = { ...values };
      this.unsaved.mark();
      this.saveSuccess.set(true);
      this.loadCache();
    };
    const update = Object.keys(patch).length > 0 ? this.cachesService.updateCache(this.cacheName, patch) : of('');

    update.subscribe({
      next: () => {
        if (!visibilityChanged) return finish();
        const visibilityCall = values.public
          ? this.cachesService.setCachePublic(this.cacheName)
          : this.cachesService.setCachePrivate(this.cacheName);
        visibilityCall.subscribe({
          next: finish,
          error: (error: unknown) => {
            this.saveError.set(apiErrorMessage(error, 'Failed to update visibility.'));
            this.saving.set(false);
            this.loadCache();
          },
        });
      },
      error: (error: unknown) => {
        this.saveError.set(apiErrorMessage(error, 'Failed to save settings.'));
        this.saving.set(false);
      },
    });
  }

  toggleActive(): void {
    const currentCache = this.cache();
    if (!currentCache) return;

    this.toggling.set(true);
    const action = currentCache.active
      ? this.cachesService.deactivateCache(this.cacheName)
      : this.cachesService.activateCache(this.cacheName);

    action.subscribe({
      next: () => {
        this.toggling.set(false);
        this.notify.success(currentCache.active ? 'Cache deactivated' : 'Cache activated');
        this.loadCache();
      },
      error: (error: unknown) => {
        this.notify.failure(currentCache.active ? 'Cache deactivation failed' : 'Cache activation failed', error);
        this.toggling.set(false);
      },
    });
  }

  deleteCache(): void {
    this.confirm
      .confirm({
        title: 'Delete cache',
        target: this.cache()?.display_name ?? this.cacheName,
        consequence: "This action can't be undone.",
        confirmLabel: 'Delete',
        failure: 'Cache deletion failed.',
        action: () => this.cachesService.deleteCache(this.cacheName),
      })
      .subscribe(() => {
        this.notify.success('Cache deleted');
        this.router.navigateByUrl(links.caches());
      });
  }

}
