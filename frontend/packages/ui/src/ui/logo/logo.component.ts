/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, ChangeDetectionStrategy } from '@angular/core';

/// The wordmark. One file for both themes, masked so it takes the current text
/// colour: two drawings drift apart in weight and size, one cannot.
@Component({
  selector: 'gr-logo',
  standalone: true,
  template: '<span class="mark" role="img" aria-label="Gradient"></span>',
  styleUrl: './logo.component.scss',
  changeDetection: ChangeDetectionStrategy.Eager,
})
export class LogoComponent {}
