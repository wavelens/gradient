/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, inject, signal } from '@angular/core';
import { CommonModule } from '@angular/common';
import { ActivatedRoute, RouterModule } from '@angular/router';
import { FormsModule } from '@angular/forms';
import { forkJoin, of } from 'rxjs';
import { catchError } from 'rxjs/operators';
import { DialogModule } from 'primeng/dialog';
import { ButtonModule } from 'primeng/button';
import { InputTextModule } from 'primeng/inputtext';
import { InputNumberModule } from 'primeng/inputnumber';
import { CheckboxModule } from 'primeng/checkbox';
import { ServersService } from '@core/services/servers.service';
import { Server } from '@core/models';
import { LoadingSpinnerComponent } from '@shared/components/loading-spinner/loading-spinner.component';

@Component({
  selector: 'app-servers',
  standalone: true,
  imports: [
    CommonModule,
    RouterModule,
    FormsModule,
    DialogModule,
    ButtonModule,
    InputTextModule,
    InputNumberModule,
    CheckboxModule,
    LoadingSpinnerComponent,
  ],
  templateUrl: './servers.component.html',
  styleUrl: './servers.component.scss',
})
export class ServersComponent implements OnInit {
  private route = inject(ActivatedRoute);
  private serversService = inject(ServersService);

  loading = signal(true);
  creating = signal(false);
  saving = signal(false);
  deletingId = signal<string | null>(null);
  testingId = signal<string | null>(null);
  testResult = signal<{ id: string; ok: boolean; message: string } | null>(null);
  showEditDialog = signal(false);
  editServer: Server | null = null;
  editForm = { display_name: '', host: '', port: 22, username: '', architectures: [] as string[], features: '', max_concurrent_builds: 1 };

  orgName = '';
  servers = signal<Server[]>([]);
  serversTotal = signal(0);
  serversPage = signal(1);
  showCreateDialog = signal(false);
  errorMessage = signal<string | null>(null);

  availableArchitectures = [
    { label: 'x86_64-linux', value: 'x86_64-linux' },
    { label: 'aarch64-linux', value: 'aarch64-linux' },
    { label: 'x86_64-darwin', value: 'x86_64-darwin' },
    { label: 'aarch64-darwin', value: 'aarch64-darwin' },
  ];

  newServer = {
    name: '',
    display_name: '',
    host: '',
    port: 22,
    username: 'root',
    architectures: [] as string[],
    features: 'nixos-test,benchmark,big-parallel,kvm',
    max_concurrent_builds: 1,
  };

  ngOnInit(): void {
    this.orgName = this.route.snapshot.paramMap.get('org') || '';
    this.loadServers();
  }

  loadServers(page = this.serversPage()): void {
    this.loading.set(true);
    this.serversService.getServers(this.orgName, page).subscribe({
      next: (result) => {
        this.serversTotal.set(result.total);
        this.serversPage.set(result.page);
        if (result.items.length === 0) {
          this.servers.set([]);
          this.loading.set(false);
          return;
        }
        forkJoin(
          result.items.map(item =>
            this.serversService.getServer(this.orgName, item.name).pipe(
              catchError(() => of(null))
            )
          )
        ).subscribe({
          next: (results) => {
            this.servers.set(results.filter((s): s is Server => s !== null));
            this.loading.set(false);
          },
          error: () => this.loading.set(false),
        });
      },
      error: (error) => {
        console.error('Failed to load servers:', error);
        this.loading.set(false);
      },
    });
  }

  openCreateDialog(): void {
    this.newServer = { name: '', display_name: '', host: '', port: 22, username: 'root', architectures: [], features: 'nixos-test,benchmark,big-parallel,kvm', max_concurrent_builds: 1 };
    this.errorMessage.set(null);
    this.showCreateDialog.set(true);
  }

  createServer(): void {
    if (!this.newServer.name || !this.newServer.host || this.newServer.architectures.length === 0) return;
    this.creating.set(true);
    this.errorMessage.set(null);
    const features = this.newServer.features
      .split(',')
      .map(f => f.trim())
      .filter(f => f.length > 0);
    this.serversService.createServer(this.orgName, {
      name: this.newServer.name,
      display_name: this.newServer.display_name || this.newServer.name,
      host: this.newServer.host,
      port: this.newServer.port,
      username: this.newServer.username,
      architectures: this.newServer.architectures,
      features,
      max_concurrent_builds: this.newServer.max_concurrent_builds,
    }).subscribe({
      next: () => {
        this.creating.set(false);
        this.showCreateDialog.set(false);
        this.loadServers();
      },
      error: (error) => {
        this.errorMessage.set(error.message || 'Failed to create server.');
        this.creating.set(false);
      },
    });
  }

  openEditDialog(server: Server): void {
    this.editServer = server;
    this.editForm = {
      display_name: server.display_name || server.name,
      host: server.host,
      port: server.port,
      username: server.username,
      architectures: [...(server.architectures || [])],
      features: (server.features || []).join(', '),
      max_concurrent_builds: server.max_concurrent_builds || 1,
    };
    this.errorMessage.set(null);
    this.showEditDialog.set(true);
  }

  saveEdit(): void {
    if (!this.editServer || !this.editForm.host) return;
    this.saving.set(true);
    this.errorMessage.set(null);
    const editFeatures = this.editForm.features
      .split(',')
      .map(f => f.trim())
      .filter(f => f.length > 0);
    this.serversService.patchServer(this.orgName, this.editServer.name, {
      display_name: this.editForm.display_name,
      host: this.editForm.host,
      port: this.editForm.port,
      username: this.editForm.username,
      architectures: this.editForm.architectures,
      features: editFeatures,
      max_concurrent_builds: this.editForm.max_concurrent_builds,
    }).subscribe({
      next: () => {
        this.saving.set(false);
        this.showEditDialog.set(false);
        this.loadServers();
      },
      error: (error) => {
        this.errorMessage.set(error.message || 'Failed to save changes.');
        this.saving.set(false);
      },
    });
  }

  checkConnection(server: Server): void {
    this.testingId.set(server.id);
    this.testResult.set(null);
    this.serversService.checkConnection(this.orgName, server.name).subscribe({
      next: () => {
        this.testResult.set({ id: server.id, ok: true, message: 'Connection successful' });
        this.testingId.set(null);
        this.loadServers();
      },
      error: (error) => {
        this.testResult.set({ id: server.id, ok: false, message: error.message || 'Connection failed' });
        this.testingId.set(null);
      },
    });
  }

  deleteServer(server: Server): void {
    this.deletingId.set(server.id);
    this.serversService.deleteServer(this.orgName, server.name).subscribe({
      next: () => {
        this.deletingId.set(null);
        this.loadServers();
      },
      error: (error) => {
        console.error('Failed to delete server:', error);
        this.deletingId.set(null);
      },
    });
  }

  stripSpaces(field: 'newFeatures' | 'editFeatures', value: string): void {
    const stripped = value.replace(/\s/g, '');
    if (field === 'newFeatures') {
      this.newServer.features = stripped;
    } else {
      this.editForm.features = stripped;
    }
  }

  isArchSelected(arch: string): boolean {
    return this.newServer.architectures.includes(arch);
  }

  toggleArch(arch: string): void {
    const idx = this.newServer.architectures.indexOf(arch);
    if (idx >= 0) {
      this.newServer.architectures.splice(idx, 1);
    } else {
      this.newServer.architectures.push(arch);
    }
  }

  isEditArchSelected(arch: string): boolean {
    return this.editForm.architectures.includes(arch);
  }

  toggleEditArch(arch: string): void {
    const idx = this.editForm.architectures.indexOf(arch);
    if (idx >= 0) {
      this.editForm.architectures.splice(idx, 1);
    } else {
      this.editForm.architectures.push(arch);
    }
  }
}
