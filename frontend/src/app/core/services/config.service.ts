/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable, inject } from '@angular/core';
import { HttpClient } from '@angular/common/http';
import { firstValueFrom } from 'rxjs';
import { environment } from '@environments/environment';

interface ServerConfig {
  oidc_enabled: boolean;
  registration_disabled: boolean;
  email_verification_enabled: boolean;
}

@Injectable({ providedIn: 'root' })
export class ConfigService {
  private http = inject(HttpClient);

  oidcEnabled = false;
  registrationDisabled = false;
  emailVerificationEnabled = false;

  load(): Promise<void> {
    return firstValueFrom(
      this.http.get<{ error: boolean; message: ServerConfig }>(
        `${environment.apiUrl}/config`
      )
    )
      .then((res) => {
        if (!res.error) {
          this.oidcEnabled = res.message.oidc_enabled;
          this.registrationDisabled = res.message.registration_disabled;
          this.emailVerificationEnabled = res.message.email_verification_enabled;
        }
      })
      .catch(() => {
        // Keep defaults if config fetch fails
      });
  }
}
