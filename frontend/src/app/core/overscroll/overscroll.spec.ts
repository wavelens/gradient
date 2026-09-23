/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { OVERSCROLL_BOTTOM, followOverscroll } from './overscroll';

function page(scrollHeight: number, innerHeight: number) {
  const root = document.documentElement;
  Object.defineProperty(root, 'scrollHeight', { configurable: true, value: scrollHeight });
  Object.defineProperty(window, 'innerHeight', { configurable: true, value: innerHeight });
  const scrollTo = (y: number) => {
    Object.defineProperty(window, 'scrollY', { configurable: true, value: y });
    window.dispatchEvent(new Event('scroll'));
  };
  return { root, scrollTo };
}

describe('followOverscroll', () => {
  let stop: () => void;
  afterEach(() => stop?.());

  it('paints the header color in the top half and the footer color in the bottom half', () => {
    const { root, scrollTo } = page(3000, 1000);
    scrollTo(0);
    stop = followOverscroll(window);
    expect(root.classList.contains(OVERSCROLL_BOTTOM)).toBe(false);
    scrollTo(1500);
    expect(root.classList.contains(OVERSCROLL_BOTTOM)).toBe(true);
    scrollTo(200);
    expect(root.classList.contains(OVERSCROLL_BOTTOM)).toBe(false);
  });

  it('follows a bounce on a page that does not scroll', () => {
    const { root, scrollTo } = page(800, 1000);
    scrollTo(0);
    stop = followOverscroll(window);
    scrollTo(30);
    expect(root.classList.contains(OVERSCROLL_BOTTOM)).toBe(true);
    scrollTo(-30);
    expect(root.classList.contains(OVERSCROLL_BOTTOM)).toBe(false);
  });

  it('stops listening once stopped', () => {
    const { root, scrollTo } = page(3000, 1000);
    scrollTo(0);
    followOverscroll(window)();
    scrollTo(2000);
    expect(root.classList.contains(OVERSCROLL_BOTTOM)).toBe(false);
  });
});
