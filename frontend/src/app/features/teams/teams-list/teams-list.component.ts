/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, OnInit, inject, signal } from '@angular/core';
import { FormsModule } from '@angular/forms';
import { Router, RouterModule } from '@angular/router';
import { TeamsService } from '@core/services/teams.service';
import { ConfigService } from '@core/services/config.service';
import { AuthService } from '@core/services/auth.service';
import { TeamSummary } from '@core/models';
import {
  BadgeComponent,
  ButtonComponent,
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
  selector: 'app-teams-list',
  standalone: true,
  imports: [
    FormsModule,
    RouterModule,
    BadgeComponent,
    ButtonComponent,
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
  templateUrl: './teams-list.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
})
export class TeamsListComponent implements OnInit {
  private teams = inject(TeamsService);
  private router = inject(Router);
  private config = inject(ConfigService);
  private authService = inject(AuthService);

  get canCreateTeam(): boolean {
    if (!this.authService.isAuthenticated()) return false;
    return this.config.canCreate(this.config.createTeam, this.authService.user()?.superuser === true);
  }

  loading = signal(true);
  items = signal<TeamSummary[]>([]);
  showCreate = signal(false);
  creating = signal(false);
  error = signal<string | null>(null);
  form = { name: '', displayName: '' };

  ngOnInit(): void {
    this.teams.list().subscribe({
      next: (items) => {
        this.items.set(items);
        this.loading.set(false);
      },
      error: () => this.loading.set(false),
    });
  }

  create(): void {
    if (!this.form.name || !this.form.displayName) return;
    this.creating.set(true);
    this.error.set(null);
    this.teams.create(this.form.name, this.form.displayName).subscribe({
      next: () => this.router.navigate(['/team', this.form.name]),
      error: (err: Error) => {
        this.error.set(err.message || 'Failed to create the team.');
        this.creating.set(false);
      },
    });
  }
}
