/*
 * SPDX-FileCopyrightText: 2024 Wavelens UG <info@wavelens.io>
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

function dropdownFunction() {
  const dropdown = document.getElementById("dropdownHeader");
  const tooltip = document.getElementById("tooltip");

  dropdown.classList.toggle("show");

  if (dropdown.classList.contains("show")) {
      tooltip.classList.add("hidden");
  } else {
      tooltip.classList.remove("hidden");
  }
}

window.onclick = function (event) {
  if (!event.target.matches('.dropbtn')) {
      var dropdowns = document.getElementsByClassName("dropdown-content");
      var tooltip = document.getElementById("tooltip");

      for (let i = 0; i < dropdowns.length; i++) {
          var openDropdown = dropdowns[i];
          if (openDropdown.classList.contains('show')) {
              openDropdown.classList.remove('show');
              tooltip.classList.remove("hidden");
          }
      }
  }
};
