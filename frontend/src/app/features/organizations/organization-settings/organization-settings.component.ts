/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, inject, signal } from '@angular/core';
import { CommonModule } from '@angular/common';
import { ActivatedRoute, Router, RouterModule } from '@angular/router';
import { FormsModule } from '@angular/forms';
import { DialogModule } from 'primeng/dialog';
import { DividerModule } from 'primeng/divider';
import { ButtonModule } from 'primeng/button';
import { InputTextModule } from 'primeng/inputtext';
import { TextareaModule } from 'primeng/textarea';
import { OrganizationsService } from '@core/services/organizations.service';
import { OrgAccessService } from '@core/services/org-access.service';
import { LoadingSpinnerComponent } from '@shared/components/loading-spinner/loading-spinner.component';
import { AccessBannerComponent, WritableDirective, ManagedDisableDirective } from '@shared/access';
import { Organization, AccessState } from '@core/models';

@Component({
  selector: 'app-organization-settings',
  standalone: true,
  imports: [
    CommonModule,
    RouterModule,
    FormsModule,
    DialogModule,
    DividerModule,
    ButtonModule,
    InputTextModule,
    TextareaModule,
    LoadingSpinnerComponent,
    AccessBannerComponent,
    WritableDirective,
    ManagedDisableDirective,
  ],
  templateUrl: './organization-settings.component.html',
  styleUrl: './organization-settings.component.scss',
})
export class OrganizationSettingsComponent implements OnInit {
  private route = inject(ActivatedRoute);
  private router = inject(Router);
  private organizationsService = inject(OrganizationsService);
  private orgAccess = inject(OrgAccessService);

  access = signal<AccessState>({ managed: false, canEdit: false });

  loading = signal(true);
  saving = signal(false);
  deleting = signal(false);
  sshLoading = signal(true);
  generatingSSH = signal(false);

  organization = signal<Organization | null>(null);
  sshKey = signal<string>('');

  showDeleteDialog = signal(false);
  showRegenerateKeyDialog = signal(false);
  saveError = signal<string | null>(null);
  saveSuccess = signal(false);

  orgName = '';

  formData = {
    display_name: '',
    description: '',
    public: false,
  };

  ngOnInit(): void {
    this.orgName = this.route.snapshot.paramMap.get('org') || '';
    this.orgAccess.forOrg(this.orgName).then((s) => this.access.set(s));
    this.loadOrganization();
    this.loadSSHKey();
  }

  loadOrganization(): void {
    this.loading.set(true);
    this.organizationsService.getOrganization(this.orgName).subscribe({
      next: (org) => {
        this.organization.set(org);
        this.formData = {
          display_name: org.display_name,
          description: org.description,
          public: org.public,
        };
        this.loading.set(false);
      },
      error: (error) => {
        console.error('Failed to load organization:', error);
        this.loading.set(false);
      },
    });
  }

  loadSSHKey(): void {
    this.sshLoading.set(true);
    this.organizationsService.getSSHKey(this.orgName).subscribe({
      next: (key) => {
        this.sshKey.set(key);
        this.sshLoading.set(false);
      },
      error: (error) => {
        console.error('Failed to load SSH key:', error);
        this.sshLoading.set(false);
      },
    });
  }

  saveSettings(): void {
    this.saving.set(true);
    this.saveError.set(null);
    this.saveSuccess.set(false);
    const visibilityCall = this.formData.public
      ? this.organizationsService.setPublic(this.orgName)
      : this.organizationsService.setPrivate(this.orgName);

    this.organizationsService.updateOrganization(this.orgName, {
      display_name: this.formData.display_name,
      description: this.formData.description,
    }).subscribe({
      next: () => {
        visibilityCall.subscribe({
          next: () => {
            this.saving.set(false);
            this.saveSuccess.set(true);
            this.loadOrganization();
          },
          error: (error) => {
            this.saveError.set(error?.message || 'Failed to update visibility.');
            this.saving.set(false);
            this.loadOrganization();
          },
        });
      },
      error: (error) => {
        this.saveError.set(error?.message || 'Failed to save settings.');
        this.saving.set(false);
      },
    });
  }

  deleteOrganization(): void {
    this.deleting.set(true);
    this.organizationsService.deleteOrganization(this.orgName).subscribe({
      next: () => {
        this.router.navigate(['/organizations']);
      },
      error: (error) => {
        console.error('Failed to delete organization:', error);
        this.deleting.set(false);
        this.showDeleteDialog.set(false);
      },
    });
  }

  confirmRegenerateSSHKey(): void {
    this.showRegenerateKeyDialog.set(false);
    this.generateSSHKey();
  }

  generateSSHKey(): void {
    this.generatingSSH.set(true);
    this.organizationsService.generateSSHKey(this.orgName).subscribe({
      next: (key) => {
        this.sshKey.set(key);
        this.generatingSSH.set(false);
      },
      error: (error) => {
        console.error('Failed to generate SSH key:', error);
        this.generatingSSH.set(false);
      },
    });
  }

  copySSHKey(): void {
    navigator.clipboard.writeText(this.sshKey());
  }
}
