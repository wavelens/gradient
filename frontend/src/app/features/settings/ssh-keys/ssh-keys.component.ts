/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, inject, signal, ChangeDetectionStrategy } from '@angular/core';
import { ServerTimestampToLocalDateTimePipe } from '@shared/text';
import { CommonModule } from '@angular/common';
import { FormsModule } from '@angular/forms';
import { UserService } from '@core/services/user.service';
import { ConfigService } from '@core/services/config.service';
import { apiErrorMessage } from '@core/services/api.service';
import { NotifyService } from '@core/services/notify.service';
import { SshKeyInfo } from '@core/models';
import { ConfirmService } from '@shared/confirm';
import {
  ButtonComponent,
  CopyFieldComponent,
  DialogComponent,
  FormFieldComponent,
  InputDirective,
  LoadStateComponent,
  PageLayoutComponent,
  RowComponent,
  RowListComponent,
} from '@gradient/ui/ui';
import { SettingsNavComponent } from '../settings-nav/settings-nav.component';

const OPENSSH_PUBLIC_KEY =
  /^(?:ssh-(?:ed25519|rsa|dss)|ecdsa-sha2-nistp(?:256|384|521)|sk-(?:ssh-ed25519|ecdsa-sha2-nistp256)@openssh\.com)\s+[A-Za-z0-9+/]+={0,3}(?:\s.*)?$/;

function sshKeyComment(publicKey: string): string {
  return publicKey.trim().split(/\s+/).slice(2).join(' ');
}

@Component({
  selector: 'app-ssh-keys',
  standalone: true,
  imports: [
    SettingsNavComponent,
    CommonModule,
    ServerTimestampToLocalDateTimePipe,
    FormsModule,
    ButtonComponent,
    CopyFieldComponent,
    DialogComponent,
    FormFieldComponent,
    InputDirective,
    LoadStateComponent,
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
  private notify = inject(NotifyService);
  private confirm = inject(ConfirmService);

  loading = signal(true);
  loadError = signal<unknown>(null);
  saving = signal(false);
  errorMessage = signal<string | null>(null);
  keys = signal<SshKeyInfo[]>([]);
  showDialog = signal(false);
  attempted = signal(false);
  formName = '';
  formPublicKey = '';
  private nameFromKey = '';

  readonly storeUrl = `ssh-ng://<project>@${location.hostname}`;
  readonly buildHostCommand = `nixos-rebuild switch --build-host ${this.storeUrl}`;
  readonly sshConfig = `Host ${location.hostname}\n  Port ${this.config.sshPort ?? 2222}\n  User <project>`;

  ngOnInit(): void {
    this.loadKeys();
  }

  loadKeys(): void {
    this.loading.set(true);
    this.loadError.set(null);
    this.userService.getSshKeys().subscribe({
      next: (keys) => {
        this.keys.set(keys);
        this.loading.set(false);
      },
      error: (error: unknown) => {
        this.loadError.set(error);
        this.loading.set(false);
      },
    });
  }

  openDialog(): void {
    this.formName = '';
    this.formPublicKey = '';
    this.nameFromKey = '';
    this.errorMessage.set(null);
    this.attempted.set(false);
    this.showDialog.set(true);
  }

  nameError(): string | null {
    return this.attempted() && !this.formName.trim() ? 'Enter a name.' : null;
  }

  publicKeyError(): string | null {
    const key = this.formPublicKey.trim();
    if (!key) return this.attempted() ? 'Paste a public key.' : null;
    return OPENSSH_PUBLIC_KEY.test(key) ? null : 'Paste an OpenSSH public key, for example the contents of ~/.ssh/id_ed25519.pub.';
  }

  onPublicKeyChange(publicKey: string): void {
    this.formPublicKey = publicKey;
    const comment = sshKeyComment(publicKey);
    const nameUntouched = this.formName === '' || this.formName === this.nameFromKey;
    if (comment && nameUntouched) {
      this.formName = comment;
      this.nameFromKey = comment;
    }
  }

  addKey(): void {
    if (this.saving()) return;
    this.attempted.set(true);
    if (this.nameError() || this.publicKeyError()) return;
    this.saving.set(true);
    this.errorMessage.set(null);
    this.userService.addSshKey(this.formName.trim(), this.formPublicKey.trim()).subscribe({
      next: () => {
        this.saving.set(false);
        this.showDialog.set(false);
        this.notify.success('Added SSH key');
        this.loadKeys();
      },
      error: (err: unknown) => {
        this.saving.set(false);
        this.errorMessage.set(apiErrorMessage(err, 'Could not add the key.'));
      },
    });
  }

  deleteKey(key: SshKeyInfo): void {
    this.confirm
      .confirm({
        title: 'Delete SSH key',
        target: key.name,
        consequence: 'Connections that use this key are refused.',
        confirmLabel: 'Delete',
        failure: 'Failed to delete the SSH key.',
        action: () => this.userService.deleteSshKey(key.id),
      })
      .subscribe(() => {
        this.notify.success('Deleted SSH key');
        this.loadKeys();
      });
  }
}
