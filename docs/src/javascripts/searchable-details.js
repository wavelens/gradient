// SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
// SPDX-License-Identifier: AGPL-3.0-only

function closeSearchableDetails() {
  document.querySelectorAll('details[data-auto-close="true"]').forEach((element) => {
    element.removeAttribute("open");
    element.removeAttribute("data-auto-close");
    element.querySelector(":scope > summary")?.setAttribute("tabindex", "-1");
  });
}

if (document.readyState === "loading") {
  document.addEventListener("DOMContentLoaded", closeSearchableDetails);
} else {
  closeSearchableDetails();
}
