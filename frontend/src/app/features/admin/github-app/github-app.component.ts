/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, HostListener, OnInit, computed, inject, signal, ChangeDetectionStrategy } from '@angular/core';
import { CommonModule } from '@angular/common';
import { ActivatedRoute, RouterModule } from '@angular/router';
import { FormsModule } from '@angular/forms';
import { AdminService } from '@core/services/admin.service';
import { NotifyService } from '@core/services/notify.service';
import {
  ButtonComponent,
  CopyFieldComponent,
  FormFieldComponent,
  InputDirective,
  LoadStateComponent,
  PageLayoutComponent,
} from '@gradient/ui/ui';
import { LabelHelpComponent } from '@shared/ui';
import { ManifestResult } from '@core/models';

@Component({
  selector: 'app-admin-github-app',
  standalone: true,
  imports: [
    LabelHelpComponent,CommonModule, RouterModule, FormsModule, ButtonComponent, InputDirective, LoadStateComponent, PageLayoutComponent, CopyFieldComponent, FormFieldComponent],
  templateUrl: './github-app.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './github-app.component.scss',
})
export class GithubAppComponent implements OnInit {
  private admin = inject(AdminService);
  private route = inject(ActivatedRoute);
  private notify = inject(NotifyService);

  host = signal<string>('');
  loading = signal(false);

  showResult = signal(false);
  credentials = signal<ManifestResult | null>(null);
  credentialsMissing = signal(false);
  credentialsLoading = signal(false);
  credentialsError = signal<unknown>(null);

  private keysVisible = computed(() => this.showResult() && this.credentials() !== null);

  private readonly leaveWarning =
    'The generated credentials are shown only once and cannot be recovered. Leave anyway?';

  ngOnInit(): void {
    const ready = this.route.snapshot.queryParamMap.get('ready');
    if (ready === '1') {
      this.showResult.set(true);
      this.loadCredentials();
    }
  }

  create(): void {
    this.loading.set(true);
    const host = this.host().trim() || undefined;
    this.admin.requestGithubAppManifest(host).subscribe({
      next: (resp) => {
        this.loading.set(false);
        this.submitManifestForm(resp.post_url, resp.manifest);
      },
      error: (err: unknown) => {
        this.loading.set(false);
        this.notify.failure('Setup for GitHub App did not start', err);
      },
    });
  }

  private submitManifestForm(action: string, manifest: unknown): void {
    const form = document.createElement('form');
    form.method = 'POST';
    form.action = action;
    form.target = '_self';
    const input = document.createElement('input');
    input.type = 'hidden';
    input.name = 'manifest';
    input.value = JSON.stringify(manifest);
    form.appendChild(input);
    document.body.appendChild(form);
    form.submit();
  }

  loadCredentials(): void {
    this.credentialsLoading.set(true);
    this.credentialsError.set(null);
    this.admin.fetchGithubAppCredentials().subscribe({
      next: (creds) => {
        this.credentials.set(creds);
        this.credentialsLoading.set(false);
      },
      error: (err) => {
        this.credentialsLoading.set(false);
        if (err?.status === 404) {
          this.credentialsMissing.set(true);
        } else {
          this.credentialsError.set(err);
        }
      },
    });
  }

  canDeactivate(): boolean {
    return !this.keysVisible() || window.confirm(this.leaveWarning);
  }

  @HostListener('window:beforeunload', ['$event'])
  onBeforeUnload(event: BeforeUnloadEvent): void {
    if (this.keysVisible()) {
      event.preventDefault();
      event.returnValue = this.leaveWarning;
    }
  }
}
