/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, inject, signal } from '@angular/core';
import { CommonModule } from '@angular/common';
import {
  FormBuilder,
  FormGroup,
  ReactiveFormsModule,
  Validators,
} from '@angular/forms';
import { ActivatedRoute, Router } from '@angular/router';
import { ApiService } from '@core/services/api.service';
import { AuthService } from '@core/services/auth.service';
import { take } from 'rxjs';

interface CliDeviceInfo {
  user_code: string;
  expires_at: string;
  user_agent: string | null;
  ip: string | null;
}

@Component({
  selector: 'app-cli-authorize',
  standalone: true,
  imports: [CommonModule, ReactiveFormsModule],
  templateUrl: './cli-authorize.component.html',
  styleUrl: './cli-authorize.component.scss',
})
export class CliAuthorizeComponent implements OnInit {
  private fb = inject(FormBuilder);
  private api = inject(ApiService);
  private auth = inject(AuthService);
  private route = inject(ActivatedRoute);
  private router = inject(Router);

  form: FormGroup;
  info = signal<CliDeviceInfo | null>(null);
  loading = signal(true);
  submitting = signal(false);
  status = signal<'pending' | 'authorized' | 'denied'>('pending');
  errorMessage = signal<string | null>(null);

  constructor() {
    this.form = this.fb.group({
      userCode: ['', [Validators.required]],
    });
  }

  ngOnInit(): void {
    const code = this.route.snapshot.queryParamMap.get('code');
    if (code) {
      this.form.patchValue({ userCode: code });
    }

    this.auth.initialized$.pipe(take(1)).subscribe(() => {
      if (!this.auth.isAuthenticated()) {
        const next = this.router.url;
        this.router.navigate(['/account/login'], {
          queryParams: { next },
        });
        return;
      }
      if (code) {
        this.lookup(code);
      } else {
        this.loading.set(false);
      }
    });
  }

  private lookup(userCode: string): void {
    this.loading.set(true);
    this.errorMessage.set(null);
    this.api
      .get<CliDeviceInfo>(`auth/cli/info?user_code=${encodeURIComponent(userCode)}`)
      .subscribe({
        next: (info) => {
          this.info.set(info);
          this.loading.set(false);
        },
        error: (e) => {
          this.errorMessage.set(e.message || 'Invalid or expired code.');
          this.loading.set(false);
        },
      });
  }

  onLookup(): void {
    const code = this.form.value.userCode?.trim().toUpperCase();
    if (code) {
      this.form.patchValue({ userCode: code });
      this.lookup(code);
    }
  }

  authorize(): void {
    const userCode = this.info()?.user_code;
    if (!userCode) return;
    this.submitting.set(true);
    this.api.post('auth/cli/authorize', { user_code: userCode }).subscribe({
      next: () => {
        this.submitting.set(false);
        this.status.set('authorized');
      },
      error: (e) => {
        this.submitting.set(false);
        this.errorMessage.set(e.message || 'Failed to authorize.');
      },
    });
  }

  deny(): void {
    const userCode = this.info()?.user_code;
    if (!userCode) return;
    this.submitting.set(true);
    this.api.post('auth/cli/deny', { user_code: userCode }).subscribe({
      next: () => {
        this.submitting.set(false);
        this.status.set('denied');
      },
      error: (e) => {
        this.submitting.set(false);
        this.errorMessage.set(e.message || 'Failed to deny.');
      },
    });
  }
}
