/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, inject, signal, ChangeDetectionStrategy } from '@angular/core';
import { CommonModule } from '@angular/common';
import { FormsModule } from '@angular/forms';
import { UserService } from '@core/services/user.service';
import { ConfigService } from '@core/services/config.service';
import { SshKey } from '@core/models';
import {
  ButtonComponent,
  CopyFieldComponent,
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

@Component({
  selector: 'app-ssh-keys',
  standalone: true,
  imports: [
    CommonModule,
    FormsModule,
    ButtonComponent,
    CopyFieldComponent,
    DialogComponent,
    EmptyStateComponent,
    FormFieldComponent,
    InputDirective,
    LoadingSpinnerComponent,
    MessageBannerComponent,
    PageLayoutComponent,
    RowComponent,
    RowListComponent,
  ],
  templateUrl: './ssh-keys.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
})
export class SshKeysComponent implements OnInit {
  private userService = inject(UserService);
  private config = inject(ConfigService);

  loading = signal(true);
  saving = signal(false);
  deletingId = signal<string | null>(null);
  errorMessage = signal<string | null>(null);
  keys = signal<SshKey[]>([]);
  showDialog = signal(false);
  formName = '';
  formPublicKey = '';

  readonly storeUrl = `ssh-ng://<project>@${location.hostname}`;
  readonly buildHostCommand = `nixos-rebuild switch --build-host ${this.storeUrl}`;
  readonly sshConfig = `Host ${location.hostname}\n  Port ${this.config.sshPort ?? 2222}\n  User <project>`;

  ngOnInit(): void {
    this.loadKeys();
  }

  loadKeys(): void {
    this.loading.set(true);
    this.userService.getSshKeys().subscribe({
      next: (keys) => {
        this.keys.set(keys);
        this.loading.set(false);
      },
      error: () => this.loading.set(false),
    });
  }

  openDialog(): void {
    this.formName = '';
    this.formPublicKey = '';
    this.errorMessage.set(null);
    this.showDialog.set(true);
  }

  addKey(): void {
    this.saving.set(true);
    this.errorMessage.set(null);
    this.userService.addSshKey(this.formName.trim(), this.formPublicKey.trim()).subscribe({
      next: () => {
        this.saving.set(false);
        this.showDialog.set(false);
        this.loadKeys();
      },
      error: (e) => {
        this.saving.set(false);
        this.errorMessage.set(e?.error?.message ?? 'Could not add the key.');
      },
    });
  }

  deleteKey(key: SshKey): void {
    this.deletingId.set(key.id);
    this.userService.deleteSshKey(key.id).subscribe({
      next: () => {
        this.deletingId.set(null);
        this.loadKeys();
      },
      error: () => this.deletingId.set(null),
    });
  }
}
