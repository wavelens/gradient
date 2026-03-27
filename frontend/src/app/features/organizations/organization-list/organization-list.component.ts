/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnDestroy, OnInit, inject, signal } from '@angular/core';
import { CommonModule } from '@angular/common';
import { RouterModule } from '@angular/router';
import { FormsModule } from '@angular/forms';
import { Subject } from 'rxjs';
import { debounceTime, switchMap } from 'rxjs/operators';
import { DialogModule } from 'primeng/dialog';
import { ButtonModule } from 'primeng/button';
import { InputTextModule } from 'primeng/inputtext';
import { TextareaModule } from 'primeng/textarea';
import { OrganizationsService } from '@core/services/organizations.service';
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
  private nameCheck$ = new Subject<string>();

  loading = signal(true);
  organizations = signal<Organization[]>([]);
  showCreateDialog = signal(false);
  creating = signal(false);
  nameCheckState = signal<'idle' | 'checking' | 'available' | 'taken'>('idle');

  newOrg = {
    name: '',
    display_name: '',
    description: '',
    public: false,
  };

  publicOrgs = signal<Organization[]>([]);
  publicLoading = signal(false);

  ngOnInit(): void {
    this.loadOrganizations();
    this.loadPublicOrganizations();
    this.nameCheck$.pipe(
      debounceTime(400),
      switchMap((name) => {
        if (!name) { this.nameCheckState.set('idle'); return []; }
        return this.organizationsService.checkOrgNameAvailable(name);
      }),
    ).subscribe((available) => {
      this.nameCheckState.set(available ? 'available' : 'taken');
    });
  }

  ngOnDestroy(): void {
    this.nameCheck$.complete();
  }

  loadOrganizations(): void {
    this.loading.set(true);
    this.organizationsService.getOrganizations().subscribe({
      next: (orgs) => {
        this.organizations.set(orgs);
        this.loading.set(false);
      },
      error: (error) => {
        console.error('Failed to load organizations:', error);
        this.loading.set(false);
      },
    });
  }

  loadPublicOrganizations(): void {
    this.publicLoading.set(true);
    this.organizationsService.getPublicOrganizations().subscribe({
      next: (orgs) => {
        this.publicOrgs.set(orgs);
        this.publicLoading.set(false);
      },
      error: () => this.publicLoading.set(false),
    });
  }

  openCreateDialog(): void {
    this.newOrg = { name: '', display_name: '', description: '', public: false };
    this.nameCheckState.set('idle');
    this.showCreateDialog.set(true);
  }

  onOrgNameChange(name: string): void {
    this.nameCheckState.set(name ? 'checking' : 'idle');
    this.nameCheck$.next(name);
  }

  createOrganization(): void {
    if (!this.newOrg.name || !this.newOrg.display_name) {
      return;
    }

    this.creating.set(true);
    this.organizationsService.createOrganization(this.newOrg).subscribe({
      next: () => {
        this.creating.set(false);
        this.showCreateDialog.set(false);
        this.loadOrganizations();
      },
      error: (error) => {
        console.error('Failed to create organization:', error);
        this.creating.set(false);
      },
    });
  }
}
