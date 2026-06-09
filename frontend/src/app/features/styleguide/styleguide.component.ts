/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, inject, signal } from '@angular/core';
import { CommonModule } from '@angular/common';
import { FormGroup, ReactiveFormsModule, Validators } from '@angular/forms';
import { ButtonModule } from 'primeng/button';
import { InputTextModule } from 'primeng/inputtext';
import { TextareaModule } from 'primeng/textarea';
import { SelectModule } from 'primeng/select';
import { CheckboxModule } from 'primeng/checkbox';
import { RadioButtonModule } from 'primeng/radiobutton';
import { TooltipModule } from 'primeng/tooltip';
import { ConfirmDialogModule } from 'primeng/confirmdialog';
import { ToastModule } from 'primeng/toast';
import { MenuModule } from 'primeng/menu';
import { PopoverModule } from 'primeng/popover';
import { DividerModule } from 'primeng/divider';
import { ConfirmationService, MenuItem, MessageService } from 'primeng/api';

import { LoadingSpinnerComponent } from '@shared/components/loading-spinner/loading-spinner.component';
import { EmptyStateComponent } from '@shared/components/empty-state/empty-state.component';
import { StatCardComponent } from '@shared/components/stat-card/stat-card.component';

import {
  FormFieldComponent,
  PasswordInputComponent,
  MessageBannerComponent,
  FormDialogComponent,
  CopyFieldComponent,
  LabelHelpComponent,
  FormFieldsBuilder,
} from '@shared/components/form';
import {
  PageLayoutComponent,
  SettingsSectionComponent,
} from '@shared/components/layout';

interface DemoCounty {
  label: string;
  value: string;
}

@Component({
  selector: 'app-styleguide',
  standalone: true,
  imports: [
    CommonModule,
    ReactiveFormsModule,
    ButtonModule,
    InputTextModule,
    TextareaModule,
    SelectModule,
    CheckboxModule,
    RadioButtonModule,
    TooltipModule,
    ConfirmDialogModule,
    ToastModule,
    MenuModule,
    PopoverModule,
    DividerModule,
    LoadingSpinnerComponent,
    EmptyStateComponent,
    StatCardComponent,
    FormFieldComponent,
    PasswordInputComponent,
    MessageBannerComponent,
    FormDialogComponent,
    CopyFieldComponent,
    LabelHelpComponent,
    PageLayoutComponent,
    SettingsSectionComponent,
  ],
  providers: [ConfirmationService, MessageService],
  templateUrl: './styleguide.component.html',
  styleUrls: ['./styleguide.component.scss', './styleguide.demos.scss'],
})
export class StyleguideComponent {
  private ff = inject(FormFieldsBuilder);
  private confirmService = inject(ConfirmationService);
  private messageService = inject(MessageService);

  // ── Color groups ───────────────────────────────────────────────────────────
  baseColors = [
    { name: '$color-white', hex: '#ffffff' },
    { name: '$color-black', hex: '#000000' },
  ];
  themeColors = [
    { name: '$color-primary', hex: '#0d1118' },
    { name: '$color-secondary', hex: '#abb0b4' },
    { name: '$color-tertiary', hex: '#21262d' },
    { name: '$color-quaternary', hex: '#050708' },
  ];
  uiColors = [
    { name: '$color-hover', hex: '#252d33' },
    { name: '$color-border', hex: '#2d333b' },
  ];
  semanticColors = [
    { name: '$color-success', hex: '#28a745' },
    { name: '$color-danger', hex: '#dc3545' },
    { name: '$color-warning', hex: '#ffc107' },
    { name: '$color-info', hex: '#17a2b8' },
  ];
  graphColors = [
    { name: '$color-graph-success', hex: '#22c55e' },
    { name: '$color-graph-danger', hex: '#ef4444' },
    { name: '$color-graph-warning', hex: '#f97316' },
    { name: '$color-graph-running', hex: '#3b82f6' },
  ];
  textColors = [
    { name: '$text-primary', hex: '#ffffff' },
    { name: '$text-secondary', hex: '#abb0b4' },
    { name: '$text-light', hex: '#818181' },
  ];

  // ── Spacing & radius ───────────────────────────────────────────────────────
  spacings = [
    { name: '$spacing-xs', size: '0.25rem' },
    { name: '$spacing-sm', size: '0.5rem' },
    { name: '$spacing-md', size: '1rem' },
    { name: '$spacing-lg', size: '1.5rem' },
    { name: '$spacing-xl', size: '2rem' },
    { name: '$spacing-xxl', size: '3rem' },
  ];
  radii = [
    { name: '$border-radius-sm', size: '5px' },
    { name: '$border-radius-md', size: '8px' },
    { name: '$border-radius-lg', size: '12px' },
  ];
  fontSizes = [
    { name: '$font-size-xs', size: '0.75rem' },
    { name: '$font-size-sm', size: '0.875rem' },
    { name: '$font-size-md', size: '1rem' },
    { name: '$font-size-lg', size: '1.25rem' },
    { name: '$font-size-xl', size: '1.5rem' },
    { name: '$font-size-xxl', size: '2rem' },
  ];

  // ── Icons ──────────────────────────────────────────────────────────────────
  materialIcons = [
    'home', 'settings', 'person', 'logout', 'check_circle', 'error', 'warning',
    'info', 'visibility', 'visibility_off', 'edit', 'delete', 'add', 'close',
    'search', 'menu', 'arrow_back', 'arrow_forward', 'expand_more', 'refresh',
  ];
  primeIcons = [
    'pi-home', 'pi-cog', 'pi-user', 'pi-sign-out', 'pi-check', 'pi-times',
    'pi-info-circle', 'pi-exclamation-triangle', 'pi-pencil', 'pi-trash',
    'pi-plus', 'pi-minus', 'pi-search', 'pi-bars', 'pi-arrow-left',
    'pi-arrow-right', 'pi-chevron-down', 'pi-refresh', 'pi-copy', 'pi-link',
  ];

  // ── TOC ────────────────────────────────────────────────────────────────────
  toc = [
    { id: 'colors', label: 'Colors' },
    { id: 'typography', label: 'Typography' },
    { id: 'spacing', label: 'Spacing & Radius' },
    { id: 'icons', label: 'Icons' },
    { id: 'buttons', label: 'Buttons' },
    { id: 'form-primitives', label: 'Form Primitives' },
    { id: 'popups', label: 'Popups & Overlays' },
    { id: 'feedback', label: 'Feedback' },
    { id: 'tables', label: 'Tables & Lists' },
    { id: 'lists', label: 'Rich Lists' },
    { id: 'grids', label: 'Grids' },
    { id: 'layouts', label: 'Layouts' },
  ];

  // ── Demo form (live primitives) ────────────────────────────────────────────
  demoForm: FormGroup = new FormGroup({
    name: this.ff.text('', { required: true, minLength: 3 }),
    email: this.ff.email(),
    password: this.ff.password('', { required: true, strength: true }),
    confirm: this.ff.confirm('password'),
    bio: this.ff.text('', { maxLength: 200 }),
    role: this.ff.text('viewer', { required: true }),
    accept: this.ff.checkbox(false),
  });
  demoForm_submitted = signal<unknown | null>(null);

  roles: DemoCounty[] = [
    { label: 'Viewer', value: 'viewer' },
    { label: 'Editor', value: 'editor' },
    { label: 'Admin', value: 'admin' },
  ];

  // ── Dialog demo ────────────────────────────────────────────────────────────
  dialogVisible = signal(false);
  dialogLoading = signal(false);
  dialogForm: FormGroup = new FormGroup({
    label: this.ff.text('', { required: true }),
  });

  // ── Sample copy values & menu items ──────────────────────────────────────
  webhookUrl = 'https://gradient.example.com/api/v1/hooks/gitea/acme/web-app';
  apiToken = 'GRAD_b3a8f9e1d2c4a6b8e0f1a3c5d7e9b1d3f5a7c9e1';

  rowMenuItems: MenuItem[] = [
    { label: 'Edit', icon: 'pi pi-pencil' },
    { label: 'Duplicate', icon: 'pi pi-clone' },
    { separator: true },
    { label: 'Delete', icon: 'pi pi-trash' },
  ];

  // ── Sample data ────────────────────────────────────────────────────────────
  sampleRows = [
    { id: 1, name: 'gradient-cache', status: 'Active', updated: '2 min ago' },
    { id: 2, name: 'binary-cache', status: 'Idle', updated: '1 h ago' },
    { id: 3, name: 'staging', status: 'Failed', updated: '3 h ago' },
  ];

  sampleWorkers = [
    {
      name: 'builder-1',
      id: '550e8400-e29b-41d4-a716-446655440001',
      arch: 'x86_64-linux',
      caps: ['fetch', 'eval', 'build'],
      status: 'connected',
      managed: true,
    },
    {
      name: 'builder-2',
      id: '550e8400-e29b-41d4-a716-446655440002',
      arch: 'aarch64-linux',
      caps: ['build'],
      status: 'offline',
      managed: false,
    },
  ];

  sampleArtefacts = [
    { type: 'doc', name: 'README.md', size: '4.2 KB' },
    { type: 'bin', name: 'gradient-worker', size: '12.4 MB' },
    { type: 'tar', name: 'docs.tar.zst', size: '128 KB' },
  ];

  // ── Demo handlers ──────────────────────────────────────────────────────────
  submitDemoForm(): void {
    if (this.demoForm.invalid) {
      this.demoForm.markAllAsTouched();
      this.demoForm_submitted.set(null);
      return;
    }
    this.demoForm_submitted.set(this.demoForm.value);
  }

  openDialog(): void {
    this.dialogForm.reset({ label: '' });
    this.dialogVisible.set(true);
  }

  submitDialog(): void {
    if (this.dialogForm.invalid) {
      this.dialogForm.markAllAsTouched();
      return;
    }
    this.dialogLoading.set(true);
    setTimeout(() => {
      this.dialogLoading.set(false);
      this.dialogVisible.set(false);
      this.toast('success', 'Saved', `Got "${this.dialogForm.value.label}".`);
    }, 800);
  }

  confirmDelete(): void {
    this.confirmService.confirm({
      message: 'Are you sure you want to delete this item?',
      header: 'Confirm Delete',
      icon: 'pi pi-exclamation-triangle',
      acceptButtonProps: { label: 'Delete', severity: 'danger' },
      rejectButtonProps: { label: 'Cancel', severity: 'secondary' },
      accept: () => this.toast('success', 'Deleted', 'Item removed.'),
    });
  }

  toast(severity: 'success' | 'info' | 'warn' | 'error', summary: string, detail: string): void {
    this.messageService.add({ severity, summary, detail });
  }

  copyHex(hex: string): void {
    navigator.clipboard.writeText(hex).catch(() => {});
  }

  scrollTo(id: string): void {
    document.getElementById(id)?.scrollIntoView({ behavior: 'smooth', block: 'start' });
  }
}
