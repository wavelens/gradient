/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, OnInit, inject, signal } from '@angular/core';
import { LoadStateComponent, PageLayoutComponent } from '@gradient/ui/ui';
import type { UserAccessSummary } from '@core/models';
import { UserService } from '@core/services/user.service';
import { UserAccessSummarySectionsComponent } from '@shared/access/user-access-summary-sections.component';
import { SettingsNavComponent } from '../settings-nav/settings-nav.component';

@Component({
  selector: 'app-settings-access',
  standalone: true,
  imports: [LoadStateComponent, PageLayoutComponent, SettingsNavComponent, UserAccessSummarySectionsComponent],
  changeDetection: ChangeDetectionStrategy.Eager,
  template: `
    <gr-page-layout width="settings" title="Access">
      <app-settings-nav slot="banner" />
      <gr-load-state [loading]="loading()" [error]="loadError()" loadingMessage="Loading access..." errorTitle="Couldn't load access" (retry)="load()">
        @if (summary(); as summary) {
          <app-user-access-summary-sections [summary]="summary" />
        }
      </gr-load-state>
    </gr-page-layout>
  `,
})
export class SettingsAccessComponent implements OnInit {
  private users = inject(UserService);

  summary = signal<UserAccessSummary | null>(null);
  loading = signal(true);
  loadError = signal<unknown>(null);

  ngOnInit(): void {
    this.load();
  }

  load(): void {
    this.loading.set(true);
    this.loadError.set(null);
    this.users.getProjectsCachesAndTeamsCurrentUserCanReach().subscribe({
      next: (summary) => {
        this.summary.set(summary);
        this.loading.set(false);
      },
      error: (error: unknown) => {
        this.loadError.set(error);
        this.loading.set(false);
      },
    });
  }
}
