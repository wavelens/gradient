/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Directive } from '@angular/core';

/// Styling hook only: the element stays a native input/textarea/select so
/// ngModel, formControlName and appManagedDisable keep working untouched.
@Directive({
  selector: 'input[grInput], textarea[grInput], select[grInput]',
  standalone: true,
  host: { class: 'gr-input' },
})
export class InputDirective {}
