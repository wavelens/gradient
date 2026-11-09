/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, computed, inject, signal, ChangeDetectionStrategy } from '@angular/core';
import { CommonModule } from '@angular/common';
import {
  FormBuilder,
  FormGroup,
  ReactiveFormsModule,
  Validators,
} from '@angular/forms';
import { ActivatedRoute, Router } from '@angular/router';
import { ApiService, apiErrorMessage } from '@core/services/api.service';
import { AuthService } from '@core/services/auth.service';
import { ConfigService } from '@core/services/config.service';
import { CliDeviceInfoResponse } from '@core/models';
import { take } from 'rxjs';
import {
  ButtonComponent,
  FormFieldComponent,
  IconComponent,
  InputDirective,
  LogoComponent,
  MessageBannerComponent,
} from '@gradient/ui/ui';
import { links } from '@shared/routing';
import { CurrentTimeSignal, serverTime } from '@shared/text';

export function formatCliConfirmationCode(raw: string): string {
  const characters = raw.toUpperCase().replace(/[\s-]/g, '');
  return characters.length === 8 ? `${characters.slice(0, 4)}-${characters.slice(4)}` : characters;
}

@Component({
  selector: 'app-cli-authorize',
  standalone: true,
  imports: [CommonModule, ReactiveFormsModule, ButtonComponent, IconComponent, FormFieldComponent, InputDirective, MessageBannerComponent, LogoComponent],
  templateUrl: './cli-authorize.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './cli-authorize.component.scss',
})
export class CliAuthorizeComponent implements OnInit {
  private fb = inject(FormBuilder);
  private api = inject(ApiService);
  private auth = inject(AuthService);
  private route = inject(ActivatedRoute);
  private router = inject(Router);
  private config = inject(ConfigService);
  private clock = inject(CurrentTimeSignal);

  form: FormGroup;
  info = signal<CliDeviceInfoResponse | null>(null);
  loading = signal(true);
  submitting = signal(false);
  status = signal<'pending' | 'authorized' | 'denied'>('pending');
  errorMessage = signal<string | null>(null);
  cliConfirmationCodeRemainingTimeText = computed(() => {
    const info = this.info();
    if (!info) return null;
    const seconds = Math.ceil((serverTime(info.expires_at).getTime() - this.clock.currentTimeMilliseconds()) / 1000);
    if (seconds <= 0) return null;
    return `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, '0')}`;
  });
  get logoUrl() { return this.config.logoUrl; }

  constructor() {
    this.form = this.fb.group({
      userCode: ['', [Validators.required]],
    });
  }

  ngOnInit(): void {
    const query = this.route.snapshot.queryParamMap.get('code');
    const code = query ? formatCliConfirmationCode(query) : null;
    if (code) {
      this.form.patchValue({ userCode: code });
    }

    this.auth.initialized$.pipe(take(1)).subscribe(() => {
      if (!this.auth.isAuthenticated()) {
        const next = this.router.url;
        this.router.navigateByUrl(links.login(next));
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
      .get<CliDeviceInfoResponse>(`auth/cli/info?user_code=${encodeURIComponent(userCode)}`)
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
    const code = formatCliConfirmationCode(this.form.value.userCode ?? '');
    if (code) {
      this.form.patchValue({ userCode: code });
      this.lookup(code);
    }
  }

  clearCodeLookupAndShowCodeForm(): void {
    this.info.set(null);
    this.status.set('pending');
    this.errorMessage.set(null);
    this.form.reset({ userCode: '' });
  }

  authorize(): void {
    const userCode = this.info()?.user_code;
    if (!userCode || this.submitting()) return;
    this.submitting.set(true);
    this.errorMessage.set(null);
    this.api.post('auth/cli/authorize', { user_code: userCode }).subscribe({
      next: () => {
        this.submitting.set(false);
        this.status.set('authorized');
      },
      error: (e: unknown) => this.showRequestFailureAndReturnToCodeForm(`Couldn't approve CLI session: ${apiErrorMessage(e, 'The server rejected the request.')}`),
    });
  }

  deny(): void {
    const userCode = this.info()?.user_code;
    if (!userCode || this.submitting()) return;
    this.submitting.set(true);
    this.errorMessage.set(null);
    this.api.post('auth/cli/deny', { user_code: userCode }).subscribe({
      next: () => {
        this.submitting.set(false);
        this.status.set('denied');
      },
      error: (e: unknown) => this.showRequestFailureAndReturnToCodeForm(`Couldn't reject the CLI session: ${apiErrorMessage(e, 'The server rejected the request.')}`),
    });
  }

  private showRequestFailureAndReturnToCodeForm(message: string): void {
    this.submitting.set(false);
    this.info.set(null);
    this.errorMessage.set(message);
  }
}
