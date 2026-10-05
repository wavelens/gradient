/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { Component } from '@angular/core';
import { Router, provideRouter } from '@angular/router';
import { provideHttpClient } from '@angular/common/http';
import { provideHttpClientTesting } from '@angular/common/http/testing';
import { App } from './app';

@Component({ template: 'page' })
class PageStub {}

describe('App', () => {
  beforeEach(async () => {
    await TestBed.configureTestingModule({
      imports: [App],
      providers: [
        provideRouter([
          { path: '', component: PageStub },
          { path: 'bare', component: PageStub, data: { hideFooter: true } },
        ]),
        provideHttpClient(),
        provideHttpClientTesting(),
      ],
    }).compileComponents();
  });

  it('renders', () => {
    const fixture = TestBed.createComponent(App);
    expect(fixture.componentInstance).toBeTruthy();
  });

  it('shows the footer only once the first page has rendered, and not on pages that hide it', async () => {
    const fixture = TestBed.createComponent(App);
    fixture.detectChanges();
    const footer = () => fixture.nativeElement.querySelector('gr-footer');
    expect(footer()).toBeNull();

    await TestBed.inject(Router).navigateByUrl('/');
    fixture.detectChanges();
    expect(footer()).not.toBeNull();

    await TestBed.inject(Router).navigateByUrl('/bare');
    fixture.detectChanges();
    expect(footer()).toBeNull();
  });
});
