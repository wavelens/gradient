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
import { AutoCompleteModule } from 'primeng/autocomplete';
import { OrganizationsService, OrgMember } from '@core/services/organizations.service';
import { UserService } from '@core/services/user.service';
import { LoadingSpinnerComponent } from '@shared/components/loading-spinner/loading-spinner.component';
import { Organization } from '@core/models';

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
    AutoCompleteModule,
    LoadingSpinnerComponent,
  ],
  templateUrl: './organization-settings.component.html',
  styleUrl: './organization-settings.component.scss',
})
export class OrganizationSettingsComponent implements OnInit {
  private route = inject(ActivatedRoute);
  private router = inject(Router);
  private organizationsService = inject(OrganizationsService);
  private userService = inject(UserService);

  loading = signal(true);
  saving = signal(false);
  deleting = signal(false);
  membersLoading = signal(true);
  addingMember = signal(false);
  removingMember = signal<string | null>(null);
  updatingRole = signal<string | null>(null);
  sshLoading = signal(true);
  generatingSSH = signal(false);
  generatingForgeSecret = signal(false);
  deletingForgeSecret = signal(false);
  showDeleteForgeSecretDialog = signal(false);
  hasForgeSecret = signal<boolean>(false);
  forgeWebhookResult = signal<{ webhook_url: string; secret: string } | null>(null);
  forgeSecretError = signal<string | null>(null);

  organization = signal<Organization | null>(null);
  members = signal<OrgMember[]>([]);
  sshKey = signal<string>('');

  showDeleteDialog = signal(false);
  showAddMemberDialog = signal(false);
  showRegenerateKeyDialog = signal(false);
  saveError = signal<string | null>(null);
  saveSuccess = signal(false);
  memberError = signal<string | null>(null);
  userSuggestions = signal<string[]>([]);

  orgName = '';

  formData = {
    display_name: '',
    description: '',
    public: false,
  };

  newMember = {
    user: '',
    role: 'Admin',
  };

  roles = ['Admin', 'Write', 'View'];

  ngOnInit(): void {
    this.orgName = this.route.snapshot.paramMap.get('org') || '';
    this.loadOrganization();
    this.loadMembers();
    this.loadSSHKey();
  }

  loadOrganization(): void {
    this.loading.set(true);
    this.organizationsService.getOrganization(this.orgName).subscribe({
      next: (org) => {
        this.organization.set(org);
        this.hasForgeSecret.set(org.forge_webhook_secret_set ?? false);
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

  loadMembers(): void {
    this.membersLoading.set(true);
    this.organizationsService.getMembers(this.orgName).subscribe({
      next: (members) => {
        this.members.set(members);
        this.membersLoading.set(false);
      },
      error: (error) => {
        console.error('Failed to load members:', error);
        this.membersLoading.set(false);
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

  onUserSearch(event: { query: string }): void {
    if (!event.query.trim()) {
      this.userSuggestions.set([]);
      return;
    }
    this.userService.searchUsers(event.query).subscribe({
      next: (users) => this.userSuggestions.set(users.map((u) => u.username)),
      error: () => this.userSuggestions.set([]),
    });
  }

  openAddMemberDialog(): void {
    this.newMember = { user: '', role: 'Admin' };
    this.memberError.set(null);
    this.showAddMemberDialog.set(true);
  }

  addMember(): void {
    if (!this.newMember.user) return;
    this.addingMember.set(true);
    this.memberError.set(null);
    this.organizationsService.addMember(this.orgName, this.newMember.user, this.newMember.role).subscribe({
      next: () => {
        this.addingMember.set(false);
        this.showAddMemberDialog.set(false);
        this.loadMembers();
      },
      error: (err) => {
        this.memberError.set(err?.error?.message || err?.message || 'Failed to add member.');
        this.addingMember.set(false);
      },
    });
  }

  updateMemberRole(username: string, role: string): void {
    this.updatingRole.set(username);
    this.organizationsService.updateMemberRole(this.orgName, username, role).subscribe({
      next: () => {
        this.updatingRole.set(null);
        this.loadMembers();
      },
      error: (error) => {
        console.error('Failed to update member role:', error);
        this.updatingRole.set(null);
        this.loadMembers();
      },
    });
  }

  removeMember(username: string): void {
    this.removingMember.set(username);
    this.organizationsService.removeMember(this.orgName, username).subscribe({
      next: () => {
        this.removingMember.set(null);
        this.loadMembers();
      },
      error: (error) => {
        console.error('Failed to remove member:', error);
        this.removingMember.set(null);
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

  generateForgeWebhookSecret(): void {
    this.generatingForgeSecret.set(true);
    this.forgeSecretError.set(null);
    this.organizationsService.generateForgeWebhookSecret(this.orgName).subscribe({
      next: (result) => {
        this.forgeWebhookResult.set(result);
        this.hasForgeSecret.set(true);
        this.generatingForgeSecret.set(false);
      },
      error: (error) => {
        this.forgeSecretError.set(error?.error?.message || error?.message || 'Failed to generate webhook secret.');
        this.generatingForgeSecret.set(false);
      },
    });
  }

  deleteForgeWebhookSecret(): void {
    this.deletingForgeSecret.set(true);
    this.forgeSecretError.set(null);
    this.organizationsService.deleteForgeWebhookSecret(this.orgName).subscribe({
      next: () => {
        this.deletingForgeSecret.set(false);
        this.showDeleteForgeSecretDialog.set(false);
        this.hasForgeSecret.set(false);
        this.forgeWebhookResult.set(null);
      },
      error: (error) => {
        this.forgeSecretError.set(error?.error?.message || error?.message || 'Failed to delete webhook secret.');
        this.deletingForgeSecret.set(false);
        this.showDeleteForgeSecretDialog.set(false);
      },
    });
  }

  copyForgeWebhookUrl(): void {
    const result = this.forgeWebhookResult();
    if (result) navigator.clipboard.writeText(result.webhook_url);
  }

  copyForgeWebhookSecret(): void {
    const result = this.forgeWebhookResult();
    if (result) navigator.clipboard.writeText(result.secret);
  }
}
