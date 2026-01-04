/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

document.addEventListener("DOMContentLoaded", function() {
    const links = document.querySelectorAll('.nav-link');
    const currentURL = window.location.href;
    links.forEach(link => {
        if (link.href === currentURL) {
            link.classList.add('active');
        }
    });
});

document.addEventListener("DOMContentLoaded", function() {
    const links = document.querySelectorAll('.settings-nav-link');
    const currentURL = window.location.href;
    links.forEach(link => {
        if (link.href === currentURL) {
            link.classList.add('active');
        }
    });
});

function toggleDropdown(openDropdownId, openTooltipId, closeDropdownId, closeTooltipId) {
    const openDropdown = document.getElementById(openDropdownId);
    const openTooltip = document.getElementById(openTooltipId);
    const closeDropdown = document.getElementById(closeDropdownId);
    const closeTooltip = document.getElementById(closeTooltipId);

    if (closeDropdown.classList.contains("show")) {
        closeDropdown.classList.remove("show");
        closeTooltip.classList.remove("hidden");
    }

    openDropdown.classList.toggle("show");

    if (openDropdown.classList.contains("show")) {
        openTooltip.classList.add("hidden");
    } else {
        openTooltip.classList.remove("hidden");
    }
}

window.onclick = function (event) {
    if (!event.target.closest(".dropdown")) {
        document.querySelectorAll(".dropdown-content").forEach(dropdown => dropdown.classList.remove("show"));
        document.querySelectorAll(".tooltiptext").forEach(tooltip => tooltip.classList.remove("hidden"));
    }
};

