document.addEventListener("DOMContentLoaded", function() {
    const links = document.querySelectorAll('.nav-link');
    const currentURL = window.location.href;
    links.forEach(link => {
        if (link.href === currentURL) {
            link.classList.add('active');
        }
    });
});