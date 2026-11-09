/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, HostListener, OnInit, computed, inject, signal, ChangeDetectionStrategy } from '@angular/core';
import { CommonModule } from '@angular/common';
import { ActivatedRoute, Router, RouterModule } from '@angular/router';
import { FormsModule } from '@angular/forms';
import { AdminService } from '@core/services/admin.service';
import { BreadcrumbsService } from '@core/services/breadcrumbs.service';
import { ConfigService } from '@core/services/config.service';
import { NotifyService } from '@core/services/notify.service';
import {
  ButtonComponent,
  CopyFieldComponent,
  FormFieldComponent,
  InputDirective,
  LoadStateComponent,
  MessageBannerComponent,
  PageLayoutComponent,
} from '@gradient/ui/ui';
import { Observable, defaultIfEmpty, of } from 'rxjs';
import { LabelHelpComponent } from '@shared/ui';
import { ConfirmService } from '@shared/confirm';
import { ManifestResult } from '@core/models';

@Component({
  selector: 'app-admin-github-app',
  standalone: true,
  imports: [
    LabelHelpComponent,CommonModule, RouterModule, FormsModule, ButtonComponent, InputDirective, LoadStateComponent, MessageBannerComponent, PageLayoutComponent, CopyFieldComponent, FormFieldComponent],
  templateUrl: './github-app.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './github-app.component.scss',
})
export class GithubAppComponent implements OnInit {
  private admin = inject(AdminService);
  protected readonly trail = inject(BreadcrumbsService).admin({ label: 'GitHub App' });
  private route = inject(ActivatedRoute);
  private router = inject(Router);
  private notify = inject(NotifyService);
  private confirm = inject(ConfirmService);

  host = signal<string>('');
  normalizedGithubEnterpriseHost = computed(() => this.host().trim().replace(/^https?:\/\//i, '').replace(/\/+$/, ''));
  githubHostInvalidCharacterErrorMessage = computed(() =>
    /^[A-Za-z0-9.-]*$/.test(this.normalizedGithubEnterpriseHost())
      ? ''
      : 'Use only letters, numbers, dots, and dashes (for example: ghe.example.com).',
  );
  protected readonly githubAppAlreadyConfigured = inject(ConfigService).githubAppEnabled;
  loading = signal(false);

  showResult = signal(false);
  credentials = signal<ManifestResult | null>(null);
  credentialsMissing = signal(false);
  credentialsLoading = signal(false);
  credentialsError = signal<unknown>(null);

  private keysVisible = computed(() => this.showResult() && this.credentials() !== null);

  private readonly leaveWarning =
    "The server only shows the generated credentials once and there's no way to recover them.";

  ngOnInit(): void {
    const ready = this.route.snapshot.queryParamMap.get('ready');
    if (ready === '1') {
      this.showResult.set(true);
      this.loadCredentials();
    }
  }

  create(): void {
    this.loading.set(true);
    const host = this.normalizedGithubEnterpriseHost() || undefined;
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

  removeReadyQueryParamAndReturnToSetupForm(): void {
    this.credentialsMissing.set(false);
    this.showResult.set(false);
    void this.router.navigate([], { relativeTo: this.route, queryParams: { ready: null }, queryParamsHandling: 'merge', replaceUrl: true });
  }

  canDeactivate():boolean | Observable<boolean> {
    if (!this.keysVisible()) return true;
    return this.confirm
      .confirm({
        title: 'Leave page',
        target: 'GitHub App credentials',
        consequence: this.leaveWarning,
        confirmLabel: 'Leave',
        severity: 'danger',
        failure: 'Failed to leave the page.',
        action: () => of(true),
      })
      .pipe(defaultIfEmpty(false));
  }

  @HostListener('window:beforeunload', ['$event'])
  onBeforeUnload(event: BeforeUnloadEvent): void {
    if (this.keysVisible()) {
      event.preventDefault();
      event.returnValue = this.leaveWarning;
    }
  }
}
