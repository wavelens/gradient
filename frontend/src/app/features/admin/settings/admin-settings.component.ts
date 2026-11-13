/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, OnInit, computed, inject, signal } from '@angular/core';
import { LoadStateComponent, TableComponent } from '@gradient/ui/ui';
import { AdminService } from '@core/services/admin.service';
import { EffectiveConfigOption, ServerOptionValueSource } from '@core/models';

const optionValueSourceLabels: Record<ServerOptionValueSource, string> = {
  flag: 'Flags for the command line',
  env: 'Environment variables',
  default: 'Default',
  unset: 'Not set',
};

@Component({
  selector: 'app-admin-settings',
  standalone: true,
  imports: [LoadStateComponent, TableComponent],
  template: `
    <p class="hint">The server options here are active. Change them in the NixOS module or environment. Then restart the server.</p>
    <gr-load-state [loading]="loading()" [error]="loadError()" [empty]="options().length === 0"
                   loadingMessage="Loading options for the server..." errorTitle="Failed to load server options" (retry)="load()">
      @for (section of serverOptionsByGroup(); track section.group) {
        <h2>{{ section.group }}</h2>
        <gr-table class="options">
          <colgroup>
            <col class="server-option-flag-column">
            <col class="environment-variable-name-column">
            <col class="current-option-value-column">
            <col class="value-source-column">
          </colgroup>
          <thead><tr><th>Flag</th><th>Environment</th><th>Value</th><th>Source</th></tr></thead>
          <tbody>
            @for (option of section.options; track option.flag) {
              <tr>
                <td><code>--{{ option.flag }}</code></td>
                <td>@if (option.env) { <code>{{ option.env }}</code> } @else { <span class="muted">-</span> }</td>
                <td>
                  @if (option.secret) {
                    <span class="muted">Hidden</span>
                  } @else if (option.value == null) {
                    <span class="muted">-</span>
                  } @else {
                    <code>{{ option.value }}</code>
                  }
                </td>
                <td>{{ sourceLabels[option.source] }}</td>
              </tr>
            }
          </tbody>
        </gr-table>
      }
    </gr-load-state>
  `,
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: '../admin-area.scss',
  styles: `
    .server-option-flag-column { width: 25%; }
    .environment-variable-name-column { width: 30%; }
    .current-option-value-column { width: 25%; }
    .value-source-column { width: 20%; }
    .options ::ng-deep table { table-layout: fixed; min-width: 40rem; }
    .options code { overflow-wrap: anywhere; }
  `,
})
export class AdminSettingsComponent implements OnInit {
  private admin = inject(AdminService);

  protected readonly sourceLabels = optionValueSourceLabels;

  options = signal<EffectiveConfigOption[]>([]);
  loading = signal(true);
  loadError = signal<unknown>(null);

  readonly serverOptionsByGroup = computed(() => {
    const groups = new Map<string, EffectiveConfigOption[]>();
    for (const option of this.options()) groups.set(option.group, [...(groups.get(option.group) ?? []), option]);
    return [...groups].map(([group, options]) => ({ group, options }));
  });

  ngOnInit(): void {
    this.load();
  }

  load(): void {
    this.loading.set(true);
    this.loadError.set(null);
    this.admin.getServerOptionsWithValuesAndSources().subscribe({
      next: (options) => {
        this.options.set(options);
        this.loading.set(false);
      },
      error: (error: unknown) => {
        this.loadError.set(error);
        this.loading.set(false);
      },
    });
  }
}
