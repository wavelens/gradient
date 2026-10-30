/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, effect, inject, input, model, output, signal, untracked } from '@angular/core';
import { FormsModule } from '@angular/forms';
import {
  ButtonComponent,
  DialogComponent,
  FormFieldComponent,
  InputDirective,
  MessageBannerComponent,
  SelectComponent,
} from '@gradient/ui/ui';
import { CachesService } from '@core/services/caches.service';
import type { MakeCacheRequest } from '@core/models';
import { nameAvailability } from './name-availability';
import { NameFieldsComponent } from './name-fields.component';

const EMPTY_CACHE: MakeCacheRequest = { name: '', display_name: '', description: '', priority: 50, local_priority: null, public: false };

@Component({
  selector: 'app-create-cache-dialog',
  standalone: true,
  imports: [FormsModule, ButtonComponent, DialogComponent, FormFieldComponent, InputDirective, MessageBannerComponent, SelectComponent, NameFieldsComponent],
  changeDetection: ChangeDetectionStrategy.Eager,
  template: `
    <gr-dialog [header]="header()" [(visible)]="visible" width="500px">
      @if (error(); as message) {
        <gr-message-banner type="error">{{ message }}</gr-message-banner>
      }
      <app-name-fields
        noun="cache"
        [names]="names"
        [(displayName)]="cache.display_name"
        [(name)]="cache.name"
        (submitted)="create()"
      />
      <gr-form-field label="Description" for="cache-desc">
        <textarea grInput id="cache-desc" [(ngModel)]="cache.description" placeholder="Describe your cache..." rows="3"></textarea>
      </gr-form-field>
      <gr-form-field label="Visibility" for="cache-visibility" hint="Public caches are visible and readable by all users">
        <gr-select
          inputId="cache-visibility"
          [options]="visibilityOptions"
          optionLabel="label"
          optionValue="value"
          [(ngModel)]="cache.public"
        />
      </gr-form-field>
      <div grDialogFooter>
        <button grButton label="Cancel" severity="secondary" (click)="visible.set(false)" [disabled]="creating()"></button>
        <button
          grButton
          [label]="submitLabel()"
          (click)="create()"
          [disabled]="!cache.name || !cache.display_name || names.blocksSubmit() || creating()"
          [loading]="creating()"
        ></button>
      </div>
    </gr-dialog>
  `,
})
export class CreateCacheDialogComponent {
  private caches = inject(CachesService);

  visible = model(false);
  header = input('Create Cache');
  submitLabel = input('Create');
  created = output<string>();

  protected readonly visibilityOptions = [
    { label: 'Private', value: false },
    { label: 'Public', value: true },
  ];
  protected names = nameAvailability((name) => this.caches.checkCacheNameAvailable(name));
  protected cache: MakeCacheRequest = { ...EMPTY_CACHE };
  protected creating = signal(false);
  protected error = signal<string | null>(null);

  constructor() {
    effect(() => {
      if (!this.visible()) return;
      untracked(() => {
        this.cache = { ...EMPTY_CACHE };
        this.names.reset();
        this.error.set(null);
      });
    });
  }

  protected create(): void {
    if (!this.cache.name || !this.cache.display_name || this.creating()) return;
    this.creating.set(true);
    this.error.set(null);
    this.caches.createCache(this.cache).subscribe({
      next: () => {
        this.creating.set(false);
        this.visible.set(false);
        this.created.emit(this.cache.name);
      },
      error: (error: Error) => {
        this.error.set(error?.message || 'Failed to create cache.');
        this.creating.set(false);
      },
    });
  }
}
