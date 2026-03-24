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
  deletingId = signal<string | null>(null);

  orgName = '';
  servers = signal<Server[]>([]);
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
    features: '',
  };

  ngOnInit(): void {
    this.orgName = this.route.snapshot.paramMap.get('org') || '';
    this.loadServers();
  }

  loadServers(): void {
    this.loading.set(true);
    this.serversService.getServers(this.orgName).subscribe({
      next: (list) => {
        if (list.length === 0) {
          this.servers.set([]);
          this.loading.set(false);
          return;
        }
        forkJoin(
          list.map(item =>
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
    this.newServer = { name: '', display_name: '', host: '', port: 22, username: 'root', architectures: [], features: '' };
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
}
