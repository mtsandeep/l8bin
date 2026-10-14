// Shared site nav — mobile menu toggle. Loaded on every page that uses the
// common header (index.html, quickstart.html, ...).
(function() {
  const btn = document.getElementById('mobile-menu-btn');
  const menu = document.getElementById('mobile-menu');
  const b1 = document.getElementById('mhb-1');
  const b2 = document.getElementById('mhb-2');
  const b3 = document.getElementById('mhb-3');
  let open = false;

  if (btn && menu) {
    btn.addEventListener('click', () => {
      open = !open;
      menu.classList.toggle('hidden', !open);
      b1.style.transform = open ? 'translateY(6px) rotate(45deg)' : '';
      b2.style.opacity   = open ? '0' : '1';
      b3.style.transform = open ? 'translateY(-6px) rotate(-45deg)' : '';
    });

    function closeMenu() {
      open = false;
      menu.classList.add('hidden');
      b1.style.transform = b3.style.transform = '';
      b2.style.opacity = '1';
    }

    menu.querySelectorAll('a').forEach(a => a.addEventListener('click', closeMenu));
    document.addEventListener('click', (e) => {
      if (open && !btn.closest('nav').contains(e.target)) closeMenu();
    });
  }
})();
