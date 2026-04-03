/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnDestroy, OnInit, inject, signal } from '@angular/core';
import { CommonModule } from '@angular/common';
import { RouterModule } from '@angular/router';
import { FormsModule } from '@angular/forms';
import { Subject, EMPTY } from 'rxjs';
import { debounceTime, switchMap } from 'rxjs/operators';
import { DialogModule } from 'primeng/dialog';
import { ButtonModule } from 'primeng/button';
import { InputTextModule } from 'primeng/inputtext';
import { TextareaModule } from 'primeng/textarea';
import { OrganizationsService } from '@core/services/organizations.service';
import { AuthService } from '@core/services/auth.service';
import { LoadingSpinnerComponent } from '@shared/components/loading-spinner/loading-spinner.component';
import { EmptyStateComponent } from '@shared/components/empty-state/empty-state.component';
import { Organization } from '@core/models';

@Component({
  selector: 'app-organization-list',
  standalone: true,
  imports: [
    CommonModule,
    RouterModule,
    FormsModule,
    DialogModule,
    ButtonModule,
    InputTextModule,
    TextareaModule,
    LoadingSpinnerComponent,
    EmptyStateComponent,
  ],
  templateUrl: './organization-list.component.html',
  styleUrl: './organization-list.component.scss',
})
export class OrganizationListComponent implements OnInit, OnDestroy {
  private organizationsService = inject(OrganizationsService);
  protected authService = inject(AuthService);
  private nameCheck$ = new Subject<string>();

  loading = signal(true);
  organizations = signal<Organization[]>([]);
  orgsTotal = signal(0);
  orgsPage = signal(1);
  showCreateDialog = signal(false);
  creating = signal(false);
  createError = signal<string | null>(null);
  nameCheckState = signal<'idle' | 'invalid' | 'checking' | 'available' | 'taken'>('idle');

  newOrg = {
    name: '',
    display_name: '',
    description: '',
    public: false,
  };

  publicOrgs = signal<Organization[]>([]);
  publicTotal = signal(0);
  publicPage = signal(1);
  publicLoading = signal(false);

  ngOnInit(): void {
    if (this.authService.isAuthenticated()) {
      this.loadOrganizations();
    } else {
      this.loading.set(false);
    }
    this.loadPublicOrganizations();
    this.nameCheck$.pipe(
      debounceTime(400),
      switchMap((name) => name ? this.organizationsService.checkOrgNameAvailable(name) : EMPTY),
    ).subscribe((available) => {
      this.nameCheckState.set(available ? 'available' : 'taken');
    });
  }

  ngOnDestroy(): void {
    this.nameCheck$.complete();
  }

  loadOrganizations(page = this.orgsPage()): void {
    this.loading.set(true);
    this.organizationsService.getOrganizations(page).subscribe({
      next: (result) => {
        this.organizations.set(result.items);
        this.orgsTotal.set(result.total);
        this.orgsPage.set(result.page);
        this.loading.set(false);
      },
      error: (error) => {
        console.error('Failed to load organizations:', error);
        this.loading.set(false);
      },
    });
  }

  loadPublicOrganizations(page = this.publicPage()): void {
    this.publicLoading.set(true);
    this.organizationsService.getPublicOrganizations(page).subscribe({
      next: (result) => {
        this.publicOrgs.set(result.items);
        this.publicTotal.set(result.total);
        this.publicPage.set(result.page);
        this.publicLoading.set(false);
      },
      error: () => this.publicLoading.set(false),
    });
  }

  openCreateDialog(): void {
    this.newOrg = { name: '', display_name: '', description: '', public: false };
    this.nameCheckState.set('idle');
    this.createError.set(null);
    this.showCreateDialog.set(true);
  }

  onOrgNameChange(name: string): void {
    if (!name) { this.nameCheckState.set('idle'); this.nameCheck$.next(''); return; }
    if (!/^[a-z0-9]([a-z0-9-]*[a-z0-9])?$/.test(name)) {
      this.nameCheckState.set('invalid');
      this.nameCheck$.next(''); // cancel any pending debounce without making an API call
      return;
    }
    this.nameCheckState.set('checking');
    this.nameCheck$.next(name);
  }

  createOrganization(): void {
    if (!this.newOrg.name || !this.newOrg.display_name) {
      return;
    }

    this.creating.set(true);
    this.createError.set(null);
    this.organizationsService.createOrganization(this.newOrg).subscribe({
      next: () => {
        this.creating.set(false);
        this.showCreateDialog.set(false);
        this.loadOrganizations();
      },
      error: (error) => {
        this.createError.set(error?.message || 'Failed to create organization.');
        this.creating.set(false);
      },
    });
  }
}
