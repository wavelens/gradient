/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, ElementRef, effect, inject, input, model, output, signal, untracked, viewChild } from '@angular/core';
import { FormsModule } from '@angular/forms';
import {
  ButtonComponent,
  DialogComponent,
  FormFieldComponent,
  InputDirective,
  SelectComponent,
} from '@gradient/ui/ui';
import { CachesService } from '@core/services/caches.service';
import { apiErrorMessage } from '@core/services/api.service';
import { NotifyService } from '@core/services/notify.service';
import type { MakeCacheRequest } from '@core/models';
import { nameAvailability } from './name-availability';
import { NameFieldsComponent } from './name-fields.component';
import { dialogFormChangesSinceOpen } from './unsaved-changes';
import { injectRevealFirstInvalid } from './reveal-invalid';

const EMPTY_CACHE: MakeCacheRequest = { name: '', display_name: '', description: '', priority: 50, local_priority: null, public: false };

@Component({
  selector: 'app-create-cache-dialog',
  standalone: true,
  imports: [FormsModule, ButtonComponent, DialogComponent, FormFieldComponent, InputDirective, SelectComponent, NameFieldsComponent],
  changeDetection: ChangeDetectionStrategy.Eager,
  template: `
    <gr-dialog #cacheDialog [header]="header()" [(visible)]="visible" width="500px" [busy]="creating()" [error]="error()" [confirmDiscard]="formChanges.changed()">
      <app-name-fields
        #nameFields
        noun="cache"
        [names]="names"
        [submitAttempted]="submitAttempted()"
        [(displayName)]="cache.display_name"
        [(name)]="cache.name"
        (submitted)="create()"
        (nameFieldsChanged)="error.set(null)"
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
        <button grButton label="Cancel" severity="secondary" (click)="cacheDialog.close()" [disabled]="creating()"></button>
        <button
          grButton
          [label]="submitLabel()"
          (click)="create()"
          [disabled]="names.blocksSubmit() || creating()"
          [loading]="creating()"
        ></button>
      </div>
    </gr-dialog>
  `,
})
export class CreateCacheDialogComponent {
  private caches = inject(CachesService);
  private notify = inject(NotifyService);

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
  protected submitAttempted = signal(false);
  private nameFields = viewChild('nameFields', { read: ElementRef<HTMLElement> });
  private revealFirstInvalid = injectRevealFirstInvalid();
  protected formChanges = dialogFormChangesSinceOpen(() => this.cache);

  constructor() {
    effect(() => {
      if (!this.visible()) return;
      untracked(() => {
        this.cache = { ...EMPTY_CACHE };
        this.names.reset();
        this.submitAttempted.set(false);
        this.error.set(null);
        this.formChanges.mark();
      });
    });
  }

  protected create(): void {
    if (this.creating() || this.names.blocksSubmit()) return;
    this.submitAttempted.set(true);
    if (!this.cache.name || !this.cache.display_name) {
      this.revealFirstInvalid(() => this.nameFields()?.nativeElement);
      return;
    }
    this.creating.set(true);
    this.error.set(null);
    this.caches.createCache(this.cache).subscribe({
      next: (cache) => {
        this.creating.set(false);
        this.visible.set(false);
        this.notify.success('Cache created');
        this.created.emit(cache.name);
      },
      error: (error: unknown) => {
        this.error.set(apiErrorMessage(error, 'Failed to create cache.'));
        this.creating.set(false);
      },
    });
  }
}
