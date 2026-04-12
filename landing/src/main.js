import './style.css'

// Tab switching
const TAB_COLORS = { migrations:'#06b6d4', backup:'#d946ef', duplicate:'#f59e0b', handover:'#10b981' };

function switchTab(name) {
  document.querySelectorAll('.tab-content').forEach(el => el.classList.add('hidden'));
  document.querySelectorAll('.tab-btn').forEach(el => {
    el.style.borderLeftColor = 'transparent';
    el.classList.remove('bg-white/5');
  });
  document.getElementById('content-' + name).classList.remove('hidden');
  const btn = document.getElementById('tab-' + name);
  btn.style.borderLeftColor = TAB_COLORS[name];
  btn.classList.add('bg-white/5');
}

window.switchTab = switchTab;

function switchDeployTab(name) {
  ['cli', 'gh'].forEach(t => {
    document.getElementById('dcontent-' + t).classList.add('hidden');
    const btn = document.getElementById('dtab-' + t);
    btn.classList.remove('bg-white/10', 'text-white', 'shadow-sm');
    btn.classList.add('text-zinc-500');
  });
  document.getElementById('dcontent-' + name).classList.remove('hidden');
  const active = document.getElementById('dtab-' + name);
  active.classList.add('bg-white/10', 'text-white', 'shadow-sm');
  active.classList.remove('text-zinc-500');
}

window.switchDeployTab = switchDeployTab;

// Copy install command
function copyInstall() {
  navigator.clipboard.writeText('curl -fsSL l8b.in | bash').then(() => {
    const btn = document.getElementById('copy-btn');
    const orig = btn.innerHTML;
    btn.innerHTML = '<svg xmlns="http://www.w3.org/2000/svg" width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"><polyline points="20 6 9 17 4 12"/></svg>';
    btn.classList.add('text-emerald-400');
    setTimeout(() => { btn.innerHTML = orig; btn.classList.remove('text-emerald-400'); }, 2500);
  });
}

window.copyInstall = copyInstall;

// Animate memory bars + count-up on load
function countUp(id, target, duration) {
  const el = document.getElementById(id);
  if (!el) return;
  el.textContent = '0.0';
  const start = performance.now();
  function tick(now) {
    const p = Math.min((now - start) / duration, 1);
    el.textContent = ((1 - Math.pow(1 - p, 3)) * target).toFixed(1);
    if (p < 1) requestAnimationFrame(tick);
  }
  requestAnimationFrame(tick);
}

document.querySelectorAll('.bar-fill[data-w]').forEach(el => {
  setTimeout(() => {
    el.style.width = el.dataset.w;
    const cid = el.dataset.countId;
    const cval = parseFloat(el.dataset.countVal);
    if (cid && cval) countUp(cid, cval, 1200);
  }, 300);
});
setTimeout(() => countUp('mv-total', 43.4, 1200), 300);

// Scroll fade-in
const fadeObs = new IntersectionObserver(entries => {
  entries.forEach(e => { if (e.isIntersecting) { e.target.classList.add('visible'); fadeObs.unobserve(e.target); } });
}, { threshold: 0.1 });
document.querySelectorAll('.fade-in').forEach(el => fadeObs.observe(el));

// Terminal animation
const TERM_LINES = [
  { t:'$ l8b ship',                         c:'cmd', s:55,  p:260 },
  { t:'Deploy to: New project',              c:'ans', s:30,  p:80  },
  { t:'Project name: myapp',                 c:'ans', s:30,  p:110 },
  { t:'  :: Creating project myapp...',      c:'info', s:8,  p:320 },
  { t:'  ✔ Project created',                 c:'ok',  s:8,  p:80  },
  { t:'App port: 3000',                      c:'ans',  s:30, p:120 },
  { t:'  🔍 Detected: Node.js 20 · Express', c:'info', s:8,  p:60  },
  { t:'  ✔ Railpack image ready',            c:'ok',   s:8,  p:700 },
  { t:'  📦 Image built — 129.04 MiB',       c:'info', s:8,  p:60  },
  { t:'  🗜️  Compressed to 46.45 MiB',       c:'info', s:8,  p:180 },
  { t:'  Uploading  ▓▓▓▓▓▓▓▓▓▓▓▓  100%',    c:'info', s:18, p:320 },
  { t:'  ✔ Deploy successful!',              c:'ok',   s:8,  p:100 },
  { t:'  🌐 Live at: https://myapp.l8b.in',  c:'url',  s:10, p:0   },
];

function runTerminal() {
  const container = document.getElementById('term-lines');
  const cursor = document.getElementById('term-cursor');
  if (!container) return;
  container.innerHTML = '';
  let li = 0, ci = 0;
  function next() {
    if (li >= TERM_LINES.length) {
      setTimeout(() => { container.style.opacity='0'; setTimeout(() => { container.style.opacity='1'; runTerminal(); }, 600); }, 5000);
      return;
    }
    const line = TERM_LINES[li];
    if (ci === 0) {
      const el = document.createElement('div');
      el.className = 'tl ' + line.c;
      el.id = 'tl-' + li;
      container.appendChild(el);
    }
    const el = document.getElementById('tl-' + li);
    if (ci < line.t.length) {
      el.textContent += line.t[ci];
      ci++;
      const spd = line.s !== undefined ? line.s : (Math.random() * 8 + 3);
      setTimeout(next, spd);
    } else {
      li++; ci = 0;
      const pause = TERM_LINES[li - 1]?.p ?? 60;
      setTimeout(next, pause);
    }
  }
  container.style.transition = 'opacity .5s';
  next();
}

const termObs = new IntersectionObserver(entries => {
  if (entries[0].isIntersecting) { runTerminal(); termObs.disconnect(); }
}, { threshold: 0.3 });
const termEl = document.getElementById('term-lines');
if (termEl) termObs.observe(termEl.parentElement);

// Wake cycle
const WS_MSGS = [
  'Container is running and serving traffic.',
  'No requests for 10m — idle timer started.',
  'Container stopped. Memory freed. Routes removed.',
  'Request received → Waker is starting the container…',
];
let wsIdx = 0;
function stepWake() {
  for (let i = 0; i < 4; i++) {
    const row = document.getElementById('si-' + i);
    const panel = document.getElementById('ap-' + i);
    if (row) row.classList.remove('active');
    if (panel) panel.style.opacity = '0';
  }
  const row = document.getElementById('si-' + wsIdx);
  const panel = document.getElementById('ap-' + wsIdx);
  if (row) row.classList.add('active');
  if (panel) panel.style.opacity = '1';
  if (wsIdx === 3) {
    const prog = document.getElementById('wake-prog');
    if (prog) { prog.style.transition='none'; prog.style.width='5%'; requestAnimationFrame(() => { prog.style.transition='width 2s ease-out'; prog.style.width='88%'; }); }
  }
  const msg = document.getElementById('wake-msg');
  if (msg) msg.textContent = WS_MSGS[wsIdx];
  wsIdx = (wsIdx + 1) % 4;
}
stepWake();
setInterval(stepWake, 2400);

// Glitch title
const glitchEl = document.getElementById('glitch-title');
if (glitchEl) {
  const variants = glitchEl.dataset.variants.split('|');
  const glitchChars = '!@#$%^&*_+-[]{}|<>?~';
  const glitchColors = ['#22d3ee','#e879f9','#fbbf24','#34d399','#f472b6','#a78bfa'];
  let idx = 0;

  function setText(text) {
    glitchEl.innerHTML = '';
    [...text].forEach(ch => {
      const s = document.createElement('span');
      s.className = 'char';
      s.textContent = ch;
      if (ch === '.' || /[0-9]/.test(ch)) s.style.color = '#7c3aed';
      glitchEl.appendChild(s);
    });
  }

  function glitchTo(to) {
    const maxLen = Math.max(glitchEl.children.length, to.length);
    while (glitchEl.children.length < maxLen) { const s = document.createElement('span'); s.className='char'; glitchEl.appendChild(s); }
    while (glitchEl.children.length > maxLen) glitchEl.removeChild(glitchEl.lastChild);
    const chars = glitchEl.querySelectorAll('.char');
    let step = 0;
    const iv = setInterval(() => {
      step++;
      for (let i = 0; i < maxLen; i++) {
        const target = to[i] || '';
        if (step > 6) {
          chars[i].textContent = target;
          chars[i].style.color = (target === '.' || /[0-9]/.test(target)) ? '#7c3aed' : '';
        } else if (Math.random() < 0.6) {
          chars[i].textContent = glitchChars[Math.floor(Math.random() * glitchChars.length)];
          chars[i].style.color = glitchColors[Math.floor(Math.random() * glitchColors.length)];
        }
      }
      if (step >= 10) {
        clearInterval(iv);
        while (glitchEl.children.length > to.length) glitchEl.removeChild(glitchEl.lastChild);
      }
    }, 55);
  }

  setText(variants[0]);

  let cycleTimer = null;
  let flickerTimer = null;

  function startGlitch() {
    stopGlitch();
    cycleTimer = setInterval(() => {
      idx = (idx + 1) % variants.length;
      glitchTo(variants[idx]);
    }, 3000);
    flickerTimer = setInterval(() => {
      const chars = glitchEl.querySelectorAll('.char');
      if (chars.length && Math.random() < 0.3) {
        const c = chars[Math.floor(Math.random() * chars.length)];
        c.classList.add('glitching');
        setTimeout(() => c.classList.remove('glitching'), 100);
      }
    }, 200);
  }

  function stopGlitch() {
    clearInterval(cycleTimer);
    clearInterval(flickerTimer);
    cycleTimer = null;
    flickerTimer = null;
  }

  startGlitch();

  document.addEventListener('visibilitychange', () => {
    if (document.hidden) {
      stopGlitch();
    } else {
      startGlitch();
    }
  });
}

// Mobile menu toggle
(function() {
  const btn = document.getElementById('mobile-menu-btn');
  const menu = document.getElementById('mobile-menu');
  const b1 = document.getElementById('mhb-1');
  const b2 = document.getElementById('mhb-2');
  const b3 = document.getElementById('mhb-3');
  let open = false;

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
})();
