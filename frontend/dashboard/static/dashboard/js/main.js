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
    document.getElementById("dropdownHeader").classList.toggle("show");
}

window.onclick = function(event) {
    if (!event.target.matches('.dropbtn')) {
      var dropdowns = document.getElementsByClassName("dropdown-content");
      var i;
      for (i = 0; i < dropdowns.length; i++) {
        var openDropdown = dropdowns[i];
        if (openDropdown.classList.contains('show')) {
          openDropdown.classList.remove('show');
        }
      }
    }
  }