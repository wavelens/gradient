/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, inject, signal } from '@angular/core';
import { CommonModule } from '@angular/common';
import { ActivatedRoute, RouterModule } from '@angular/router';
import { FormsModule } from '@angular/forms';
import { ButtonModule } from 'primeng/button';
import { InputTextModule } from 'primeng/inputtext';
import { AdminService, GithubAppCredentials } from '@core/services/admin.service';

@Component({
  selector: 'app-admin-github-app',
  standalone: true,
  imports: [CommonModule, RouterModule, FormsModule, ButtonModule, InputTextModule],
  templateUrl: './github-app.component.html',
  styleUrl: './github-app.component.scss',
})
export class GithubAppComponent implements OnInit {
  private admin = inject(AdminService);
  private route = inject(ActivatedRoute);

  host = signal<string>('');
  loading = signal(false);
  errorMessage = signal<string | null>(null);

  showResult = signal(false);
  credentials = signal<GithubAppCredentials | null>(null);
  credentialsMissing = signal(false);

  ngOnInit(): void {
    const ready = this.route.snapshot.queryParamMap.get('ready');
    if (ready === '1') {
      this.showResult.set(true);
      this.loadCredentials();
    }
  }

  create(): void {
    this.loading.set(true);
    this.errorMessage.set(null);
    const host = this.host().trim() || undefined;
    this.admin.requestGithubAppManifest(host).subscribe({
      next: (resp) => {
        this.loading.set(false);
        this.submitManifestForm(resp.post_url, resp.manifest);
      },
      error: (err) => {
        this.loading.set(false);
        this.errorMessage.set(err?.error?.message ?? 'Failed to start manifest flow');
      },
    });
  }

  private submitManifestForm(action: string, manifest: Record<string, unknown>): void {
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

  private loadCredentials(): void {
    this.admin.fetchGithubAppCredentials().subscribe({
      next: (creds) => this.credentials.set(creds),
      error: (err) => {
        if (err?.status === 404) {
          this.credentialsMissing.set(true);
        } else {
          this.errorMessage.set('Failed to load credentials');
        }
      },
    });
  }

  copy(value: string): void {
    navigator.clipboard.writeText(value).catch(() => {});
  }
}
