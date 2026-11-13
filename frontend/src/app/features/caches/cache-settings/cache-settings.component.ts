/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, computed, inject, signal, ChangeDetectionStrategy } from '@angular/core';
import { CommonModule } from '@angular/common';
import { ActivatedRoute, Router } from '@angular/router';
import { FormsModule } from '@angular/forms';
import { catchError, finalize, map, of, switchMap } from 'rxjs';
import { BreadcrumbsService } from '@core/services/breadcrumbs.service';
import { CachesService } from '@core/services/caches.service';
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
  SelectComponent,
  SettingsSectionComponent,
  SubmitDirective,
} from '@gradient/ui/ui';
import {
  ManagedDisableDirective,
  SettingsAccessStateBadgeComponent,
  AccessService,
  injectSaveSettingsWithPrivateToPublicConfirmation,
} from '@shared/access';
import { injectCacheAccess, injectCacheAccessData, injectCacheAccessTo } from '@core/resolvers/inject-access';
import { CanComponentDeactivate } from '@core/guards/unsaved-changes.guard';
import { CachePublicKeyHistory, CacheResponse, CacheSubscribersResponse, PatchCacheRequest } from '@core/models';
import { ConfigService } from '@core/services/config.service';
import { LabelHelpComponent, formatEntityLabel } from '@shared/ui';
import { ConfirmService } from '@shared/confirm';
import { maxDisplayNameCharacterCount, unsavedChanges } from '@shared/forms';
import { links } from '@shared/routing';
import { plural, ServerTimestampToLocalDateTimePipe } from '@shared/text';
import { BinaryCacheSettingsNavigationComponent } from '../cache-settings-navigation/cache-settings-navigation.component';

const CACHE_DELETE_CONSEQUENCE = 'All store paths in this cache are deleted with the cache.';

export function buildCacheDeleteConsequenceText(subscribers: CacheSubscribersResponse | null): string {
  const users = subscribers?.projects.map((project) => project.project_display_name) ?? [];
  if (subscribers?.hidden) users.push(plural(subscribers.hidden, 'other project', 'other projects'));
  if (!users.length) return CACHE_DELETE_CONSEQUENCE;
  return `${CACHE_DELETE_CONSEQUENCE} Used by ${users.join(', ')}. Builds for these projects wait until they subscribe to another cache.`;
}

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
const MAX_STORAGE_GB = 2_147_483_647;

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
    BinaryCacheSettingsNavigationComponent,
    CommonModule,
    FormsModule,
    ButtonComponent,
    InputDirective,
    LoadStateComponent,
    ManagedDisableDirective, SettingsAccessStateBadgeComponent,
    PageLayoutComponent,
    FormFieldComponent,
    SettingsSectionComponent,
    SubmitDirective,
    MessageBannerComponent,
    RowComponent,
    SelectComponent,
    CheckboxComponent,
    CopyFieldComponent,
    LabelHelpComponent,
    ServerTimestampToLocalDateTimePipe,
  ],
  templateUrl: './cache-settings.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styles: `
    .save-hint {
      align-self: center;
    }
    .retired-key {
      display: flex;
      flex-wrap: wrap;
      gap: 0.5rem;
      overflow-wrap: anywhere;
      margin-top: 0.5rem;
    }
  `,
})
export class CacheSettingsComponent implements OnInit, CanComponentDeactivate {
  protected readonly maxDisplayNameCharacterCount = maxDisplayNameCharacterCount;
  private route = inject(ActivatedRoute);
  protected accessService = inject(AccessService);
  private router = inject(Router);
  private cachesService = inject(CachesService);
  private crumbs = inject(BreadcrumbsService);
  private notify = inject(NotifyService);
  private confirm = inject(ConfirmService);
  protected config = inject(ConfigService);
  private saveSettingsWithPrivateToPublicConfirmation = injectSaveSettingsWithPrivateToPublicConfirmation();

  private resolved = injectCacheAccessData();
  access = injectCacheAccess();
  deleteAccess = injectCacheAccessTo('deleteCache');
  manageKeysAccess = injectCacheAccessTo('manageCacheKeys');

  loading = signal(true);
  loadError = signal<unknown>(null);
  saving = signal(false);
  toggling = signal(false);

  cache = signal<CacheResponse | null>(null);
  cachePublicKeyHistory = signal<CachePublicKeyHistory | null>(null);
  saveError = signal<string | null>(null);

  cacheName = '';
  breadcrumb = computed(() => this.crumbs.cacheSettings(this.cacheName));

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
    this.loadPublicKeys();
  }

  private loadPublicKeys(): void {
    this.cachesService.getPublicKeysWithDates(this.cacheName).subscribe({
      next: (keys) => this.cachePublicKeyHistory.set(keys),
      error: () => this.cachePublicKeyHistory.set(null),
    });
  }

  confirmRotateCacheSigningKeyAndReloadKeyList(): void {
    this.confirm
      .confirm({
        title: 'Rotate Signing Key',
        target: this.cache()?.display_name ?? this.cacheName,
        consequence:
          'Cache signatures now come from a new key. Store paths keep the old signatures, so Nix clients that trust the old key continue to work. Add the new key to trusted-public-keys on all clients.',
        confirmLabel: 'Rotate Key',
        failure: 'Signing key rotation failed',
        severity: 'danger',
        action: () => this.cachesService.rotateCacheKeyAndReturnNewPublicKey(this.cacheName),
      })
      .subscribe(() => {
        this.notify.success('Rotated the signing key');
        this.loadPublicKeys();
      });
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
        return isWholeNumber(value, 0, MAX_STORAGE_GB)
          ? null
          : 'Enter 0 for unlimited. Otherwise, enter an integer from 1 to 2147483647 in GB.';
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
    const save = () => {
      this.saving.set(true);
      return (Object.keys(patch).length > 0 ? this.cachesService.updateCache(this.cacheName, patch) : of('')).pipe(
        switchMap(() => {
          if (!visibilityChanged) return of('');
          return values.public ? this.cachesService.setCachePublic(this.cacheName) : this.cachesService.setCachePrivate(this.cacheName);
        }),
      );
    };

    this.saveError.set(null);
    this.saveSettingsWithPrivateToPublicConfirmation({
      kind: 'cache',
      target: formatEntityLabel({ display_name: this.cache()?.display_name, name: this.cacheName }),
      becomesPublic: values.public && !this.saved.public,
      save,
    })
      .pipe(finalize(() => this.saving.set(false)))
      .subscribe({
        next: () => {
          this.saved = values;
          this.formData = { ...values };
          this.unsaved.mark();
          this.notify.success('Cache settings saved');
          this.loadCache();
        },
        error: (error: unknown) => this.saveError.set(apiErrorMessage(error, 'Failed to save settings.')),
      });
  }

  toggleActive(): void {
    const currentCache = this.cache();
    if (!currentCache) return;
    if (currentCache.active) {
      this.confirmDeactivation(currentCache.display_name);
      return;
    }

    this.toggling.set(true);
    this.cachesService.activateCache(this.cacheName).subscribe({
      next: () => {
        this.toggling.set(false);
        this.notify.success('Cache activated');
        this.loadCache();
      },
      error: (error: unknown) => {
        this.notify.failure('Cache activation failed', error);
        this.toggling.set(false);
      },
    });
  }

  private confirmDeactivation(target: string): void {
    this.confirm
      .confirm({
        title: 'Deactivate cache',
        target,
        consequence: 'Nix clients stop getting store paths from this cache until you activate it again.',
        confirmLabel: 'Deactivate',
        failure: "Couldn't deactivate cache",
        action: () => this.cachesService.deactivateCache(this.cacheName),
      })
      .subscribe(() => {
        this.notify.success('Cache deactivated');
        this.loadCache();
      });
  }

  deleteCache(): void {
    this.cachesService
      .getSubscribers(this.cacheName)
      .pipe(
        catchError(() => of(null)),
        map(buildCacheDeleteConsequenceText),
        switchMap((consequence) =>
          this.confirm.confirm({
            title: 'Delete cache',
            target: formatEntityLabel({ display_name: this.cache()?.display_name, name: this.cacheName }),
            consequence,
            confirmLabel: 'Delete',
            failure: 'Cache deletion failed.',
            isIrreversible: true,
            action: () => this.cachesService.deleteCache(this.cacheName),
          }),
        ),
      )
      .subscribe(() => {
        this.notify.success('Cache deleted');
        this.router.navigateByUrl(links.caches());
      });
  }

}
