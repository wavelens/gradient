/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

/// Injected into a page to score it against the design-system rules.
/// Every check is measurable; nothing here is a judgement call.
export const AUDIT = `() => {
  const SCALE = [12, 14, 16, 20, 24, 32, 48];
  const findings = [];
  const add = (check, detail) => findings.push({ check, detail });

  const visible = (el) => {
    const r = el.getBoundingClientRect();
    return r.width > 0 && r.height > 0 && getComputedStyle(el).visibility !== 'hidden';
  };

  const lum = (rgb) => {
    const c = rgb.map((v) => v / 255).map((v) => (v <= 0.03928 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4));
    return 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
  };
  const parse = (s) => (s.match(/[0-9.]+/g) || []).slice(0, 3).map(Number);
  const ratio = (a, b) => { const [hi, lo] = [lum(a), lum(b)].sort((x, y) => y - x); return (hi + 0.05) / (lo + 0.05); };
  const bgOf = (el) => {
    let n = el;
    while (n && n !== document.documentElement) {
      const bg = getComputedStyle(n).backgroundColor;
      if (bg && !bg.includes('rgba(0, 0, 0, 0)') && bg !== 'transparent') return parse(bg);
      n = n.parentElement;
    }
    return parse(getComputedStyle(document.body).backgroundColor);
  };

  // 1. layout does not overflow horizontally
  if (document.documentElement.scrollWidth > document.documentElement.clientWidth + 1) {
    add('no-horizontal-overflow', document.documentElement.scrollWidth + 'px wide vs ' + document.documentElement.clientWidth);
  }

  // 2. every semantic role resolves
  const root = getComputedStyle(document.documentElement);
  for (const name of ROLES) {
    if (!root.getPropertyValue(name).trim()) add('tokens-resolve', name + ' is empty');
  }

  const els = [...document.querySelectorAll('main *')].filter(visible);

  // 3. font sizes stay on the scale
  const offScale = new Map();
  for (const el of els) {
    if (!el.textContent?.trim() || el.children.length) continue;
    const size = Math.round(parseFloat(getComputedStyle(el).fontSize));
    if (!SCALE.includes(size)) offScale.set(size, (offScale.get(size) || 0) + 1);
  }
  for (const [size, count] of offScale) add('type-scale', size + 'px used ' + count + 'x');

  // 4. text meets AA against its own background
  for (const el of els) {
    const text = el.textContent?.trim();
    if (!text || el.children.length) continue;
    const cs = getComputedStyle(el);
    const size = parseFloat(cs.fontSize);
    const weight = Number(cs.fontWeight) || 400;
    const large = size >= 24 || (size >= 18.66 && weight >= 700);
    const r = ratio(parse(cs.color), bgOf(el));
    if (r < (large ? 3 : 4.5)) add('contrast', text.slice(0, 28) + ' -> ' + r.toFixed(2));
  }

  // 5. controls share one height
  const heights = new Set();
  for (const el of document.querySelectorAll('.gr-button, .gr-input')) {
    if (!visible(el) || el.classList.contains('gr-button--small') || el.closest('.copy-field')) continue;
    heights.add(Math.round(el.getBoundingClientRect().height));
  }
  if (heights.size > 1) add('control-height', [...heights].join(', ') + 'px');

  // 6. icons come from the primitive
  const bare = [...document.querySelectorAll('.material-symbols-outlined')].filter((e) => !e.closest('gr-icon'));
  if (bare.length) add('icon-primitive', bare.length + ' bare icon spans');

  // 7. interactive elements are reachable and labelled
  for (const el of document.querySelectorAll('button, a[href], input, select, textarea')) {
    if (!visible(el)) continue;
    const name = (el.textContent || '').trim() || el.getAttribute('aria-label') || el.getAttribute('title') || el.id;
    if (!name) add('accessible-name', el.tagName.toLowerCase() + '.' + (el.className || '(none)'));
  }

  return findings;
}`;
