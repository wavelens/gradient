/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { isTypingTarget } from './keyboard';

const el = (props: Record<string, unknown>) => props as unknown as EventTarget;

describe('isTypingTarget', () => {
  it('is true for editable elements', () => {
    expect(isTypingTarget(el({ tagName: 'INPUT' }))).toBe(true);
    expect(isTypingTarget(el({ tagName: 'TEXTAREA' }))).toBe(true);
    expect(isTypingTarget(el({ tagName: 'SELECT' }))).toBe(true);
    expect(isTypingTarget(el({ tagName: 'DIV', isContentEditable: true }))).toBe(true);
  });

  it('is false for non-editable targets', () => {
    expect(isTypingTarget(el({ tagName: 'DIV', isContentEditable: false }))).toBe(false);
    expect(isTypingTarget(el({ tagName: 'BODY' }))).toBe(false);
    expect(isTypingTarget(null)).toBe(false);
  });
});
