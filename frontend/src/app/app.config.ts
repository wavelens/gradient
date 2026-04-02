/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ApplicationConfig, APP_INITIALIZER, provideBrowserGlobalErrorListeners, inject, Injectable } from '@angular/core';
import { provideRouter, TitleStrategy, RouterStateSnapshot } from '@angular/router';
import { Title } from '@angular/platform-browser';
import { provideHttpClient, withInterceptors } from '@angular/common/http';
import { provideAnimations } from '@angular/platform-browser/animations';
import { providePrimeNG } from 'primeng/config';
import Aura from '@primeuix/themes/aura';

import { routes } from './app.routes';
import { authInterceptor } from '@core/interceptors/auth.interceptor';
import { errorInterceptor } from '@core/interceptors/error.interceptor';
import { ConfigService } from '@core/services/config.service';

@Injectable({ providedIn: 'root' })
class GradientTitleStrategy extends TitleStrategy {
  constructor(private readonly title: Title) { super(); }

  override updateTitle(state: RouterStateSnapshot): void {
    const routeTitle = this.buildTitle(state);
    this.title.setTitle(routeTitle ? `${routeTitle} · Gradient` : 'Gradient');
  }
}

export const appConfig: ApplicationConfig = {
  providers: [
    provideBrowserGlobalErrorListeners(),
    provideRouter(routes),
    { provide: TitleStrategy, useClass: GradientTitleStrategy },
    provideHttpClient(
      withInterceptors([authInterceptor, errorInterceptor])
    ),
    provideAnimations(),
    providePrimeNG({
      theme: {
        preset: Aura,
        options: {
          darkModeSelector: '.app-dark',
          cssLayer: false,
        }
      }
    }),
    {
      provide: APP_INITIALIZER,
      useFactory: () => {
        const configService = inject(ConfigService);
        return () => configService.load();
      },
      multi: true,
    },
  ]
};
