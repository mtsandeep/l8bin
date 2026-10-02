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
  'No requests for 15m — idle timer started.',
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

// Telemetry Stream for Memory Card
(function() {
  const card = document.getElementById('memory-card');
  if (!card) return;

  const statusText = document.getElementById('live-status');
  const statusDot = document.getElementById('live-dot');
  const statusChip = document.getElementById('status-chip');
  const compLabel = document.getElementById('mv-comparison');
  const cardLabel = document.getElementById('memory-card-label');
  const stackLabel = document.getElementById('stack-total-label');
  const sleepNote = document.getElementById('sleep-state-note');
  const loadNote = document.getElementById('under-load-note');
  const updatedAgo = document.getElementById('updated-ago');
  const spark = document.getElementById('mem-spark');
  let evtSource = null;
  let outViewTimer = null;
  let inViewTimer = null;
  let isStreaming = false;
  let retryCount = 0;
  let inView = false;
  let hasLive = false;
  let lastUpdate = 0;
  let agoTimer = null;
  const MAX_RETRIES = 3;
  const history = [];
  const HIST_MAX = 90;

  const STATIC_VALS = { 'mv-0': 9.4, 'mv-1': 1.5, 'mv-2': 32.8, 'mv-total': 43.7 };
  const STATIC_WIDTHS = { 'mv-0': '16%', 'mv-1': '3%', 'mv-2': '57%' };

  // One place decides the status chip's text, dot and colour per state.
  const CHIP_CLASSES = ['border-zinc-700', 'text-zinc-500', 'border-amber-500/30', 'text-amber-400', 'border-emerald-500/30', 'text-emerald-400'];
  const CHIP_STATES = {
    idle:       { text: 'idle snapshot', dot: 'bg-zinc-500',    pulse: false, cls: ['border-zinc-700', 'text-zinc-500'] },
    connecting: { text: 'connecting…',   dot: 'bg-amber-400',   pulse: true,  cls: ['border-amber-500/30', 'text-amber-400'] },
    live:       { text: 'live',          dot: 'bg-emerald-500', pulse: true,  cls: ['border-emerald-500/30', 'text-emerald-400'] },
    paused:     { text: 'paused',        dot: 'bg-zinc-500',    pulse: false, cls: ['border-zinc-700', 'text-zinc-500'] },
  };

  function setChip(state) {
    const s = CHIP_STATES[state];
    if (statusText) statusText.textContent = s.text;
    if (statusDot) {
      statusDot.classList.remove('hidden', 'bg-zinc-500', 'bg-amber-400', 'bg-emerald-500', 'animate-pulse');
      if (state === 'idle' || state === 'paused') statusDot.classList.add('hidden');
      statusDot.classList.add(s.dot);
      if (s.pulse) statusDot.classList.add('animate-pulse');
    }
    const pauseIcon = document.getElementById('live-pause');
    if (pauseIcon) {
      pauseIcon.classList.toggle('hidden', state !== 'paused');
      pauseIcon.classList.toggle('flex', state === 'paused');
      pauseIcon.classList.toggle('animate-pulse', state === 'paused');
    }
    if (statusChip) { statusChip.classList.remove(...CHIP_CLASSES); statusChip.classList.add(...s.cls); }
  }

  // Ease a numeric readout to its new value; tabular-nums keeps digits
  // from reflowing the layout while it animates.
  function tweenValue(el, to) {
    if (!el) return;
    const from = parseFloat(el.textContent) || 0;
    cancelAnimationFrame(el._raf);
    if (Math.abs(to - from) < 0.05) { el.textContent = to.toFixed(1); return; }
    const t0 = performance.now(), dur = 500;
    (function step(now) {
      const p = Math.min((now - t0) / dur, 1);
      el.textContent = (from + (to - from) * (1 - Math.pow(1 - p, 3))).toFixed(1);
      if (p < 1) el._raf = requestAnimationFrame(step);
    })(t0);
  }

  // Sparkline of the total over the stream's lifetime.
  function drawSpark() {
    if (!spark || !spark.clientWidth) return;
    const dpr = window.devicePixelRatio || 1;
    const w = spark.clientWidth, h = spark.clientHeight;
    if (spark.width !== Math.round(w * dpr) || spark.height !== Math.round(h * dpr)) {
      spark.width = Math.round(w * dpr);
      spark.height = Math.round(h * dpr);
    }
    const ctx = spark.getContext('2d');
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, w, h);

    if (history.length < 2) {
      // No stream yet — a faint dotted baseline where the graph will live.
      ctx.strokeStyle = 'rgba(139, 92, 246, 0.25)';
      ctx.setLineDash([2, 5]);
      ctx.beginPath();
      ctx.moveTo(0, h / 2);
      ctx.lineTo(w, h / 2);
      ctx.stroke();
      return;
    }

    const min = Math.min(...history), max = Math.max(...history);
    const pad = ((max - min) || 1) * 0.15 + 0.5;
    const lo = min - pad, hi = max + pad;
    const x = i => (i / (history.length - 1)) * w;
    const y = v => h - ((v - lo) / (hi - lo)) * h;

    ctx.beginPath();
    history.forEach((v, i) => (i ? ctx.lineTo(x(i), y(v)) : ctx.moveTo(x(i), y(v))));
    ctx.strokeStyle = '#8b5cf6';
    ctx.lineWidth = 1.5;
    ctx.setLineDash([]);
    ctx.stroke();
    ctx.lineTo(w, h);
    ctx.lineTo(0, h);
    ctx.closePath();
    const grad = ctx.createLinearGradient(0, 0, 0, h);
    grad.addColorStop(0, 'rgba(139, 92, 246, 0.22)');
    grad.addColorStop(1, 'rgba(139, 92, 246, 0)');
    ctx.fillStyle = grad;
    ctx.fill();
  }

  // "updated 4s ago" — resets on every message, hides when the stream goes stale.
  function tickAgo() {
    if (!updatedAgo || !hasLive) return;
    const s = Math.max(0, Math.round((Date.now() - lastUpdate) / 1000));
    if (s > 45) { updatedAgo.style.visibility = 'hidden'; return; }
    updatedAgo.textContent = s < 5 ? 'updated just now' : `updated ${s}s ago`;
    updatedAgo.style.visibility = 'visible';
  }

  function updateUI(data) {
    if (!data || !data.containers) return;
    retryCount = 0; // Reset on success
    
    // Filter for orchestrator, dashboard, caddy
    const targets = {
      'orchestrator': { id: 'mv-0', color: 'cyan' },
      'dashboard': { id: 'mv-1', color: 'fuchsia' },
      'caddy': { id: 'mv-2', color: 'amber' }
    };

    let totalMem = 0;
    const found = {};

    data.containers.forEach(c => {
      for (const [key, meta] of Object.entries(targets)) {
        if (c.name.toLowerCase().includes(key)) {
          const mb = (c.memory || 0) / 1024 / 1024;
          found[meta.id] = mb;
          totalMem += mb;
        }
      }
    });

    // Update individual bars and counts
    Object.values(targets).forEach(t => {
      const val = found[t.id] || 0;
      tweenValue(document.getElementById(t.id), val);
      
      const bar = document.querySelector(`.bar-fill[data-count-id="${t.id}"]`);
      if (bar) {
        // Simple scaling: 58MB = 100% for individual bars for visual impact
        const w = Math.min((val / 58) * 100, 100);
        bar.style.width = w + '%';
      }
    });

    // Update total
    tweenValue(document.getElementById('mv-total'), totalMem);

    // RAM threshold logic
    const isOver = totalMem > 58;
    if (compLabel) compLabel.style.visibility = isOver ? 'hidden' : 'visible';
    if (loadNote) loadNote.style.visibility = isOver ? 'visible' : 'hidden';

    setChip('live');
    hasLive = true;
    lastUpdate = Date.now();
    if (cardLabel) cardLabel.textContent = 'Active Memory Footprint';
    if (stackLabel) stackLabel.textContent = 'Total Active Stack';
    if (sleepNote) sleepNote.style.visibility = 'hidden';

    history.push(totalMem);
    if (history.length > HIST_MAX) history.shift();
    drawSpark();
    if (!agoTimer) agoTimer = setInterval(tickAgo, 1000);
    tickAgo();
  }

  function fallback() {
    Object.entries(STATIC_VALS).forEach(([id, val]) => {
      const el = document.getElementById(id);
      if (el) el.textContent = val.toFixed(1);
    });
    document.querySelectorAll('.bar-fill[data-count-id]').forEach(bar => {
      const id = bar.dataset.countId;
      if (STATIC_WIDTHS[id]) bar.style.width = STATIC_WIDTHS[id];
    });
    if (compLabel) compLabel.style.visibility = 'visible';
    if (loadNote) loadNote.style.visibility = 'hidden';
    if (cardLabel) cardLabel.textContent = 'Idle Memory Footprint';
    if (stackLabel) stackLabel.textContent = 'Total Resting Stack';
    if (sleepNote) sleepNote.style.visibility = 'visible';
    if (updatedAgo) updatedAgo.style.visibility = 'hidden';
    clearInterval(agoTimer);
    agoTimer = null;
    hasLive = false;
    history.length = 0;
    drawSpark();
    setChip('idle');
  }

  function startStream() {
    if (isStreaming) return;
    isStreaming = true;
    setChip('connecting');
    
    evtSource = new EventSource('/stats/stream');
    evtSource.onmessage = (e) => {
      try {
        const data = JSON.parse(e.data);
        updateUI(data);
      } catch (err) { console.error("Telemetry parse error", err); }
    };
    evtSource.onerror = () => {
      stopStream();
      retryCount++;
      
      if (retryCount >= MAX_RETRIES) {
        fallback();
        console.warn(`Telemetry failed after ${MAX_RETRIES} retries. Falling back to static data.`);
      } else {
        // Retry after 5s if still in view and under retry limit
        setTimeout(() => { if (inView) startStream(); }, 5000);
      }
    };
  }

  function stopStream(paused = false) {
    if (evtSource) {
      evtSource.close();
      evtSource = null;
    }
    isStreaming = false;
    if (paused) {
      setChip('paused');
      if (updatedAgo) updatedAgo.style.visibility = 'hidden';
    }
  }

  const obs = new IntersectionObserver(entries => {
    const entry = entries[0];
    if (entry.isIntersecting) {
      inView = true;
      clearTimeout(outViewTimer);
      // Wait for 1s of steady focus
      inViewTimer = setTimeout(() => {
        if (inView) startStream();
      }, 1000);
    } else {
      inView = false;
      clearTimeout(inViewTimer);
      // Wait for 10s of absence
      outViewTimer = setTimeout(() => {
        if (!inView) stopStream(true);
      }, 10000);
    }
  }, { threshold: 0.1 });

  obs.observe(card);

  let tabHiddenTimer = null;
  let tabVisibleTimer = null;

  document.addEventListener('visibilitychange', () => {
    if (document.hidden) {
      clearTimeout(tabVisibleTimer);
      tabHiddenTimer = setTimeout(() => {
        stopStream(true);
      }, 10000);
    } else {
      clearTimeout(tabHiddenTimer);
      tabVisibleTimer = setTimeout(() => {
        if (inView) startStream();
      }, 2000);
    }
  });

  drawSpark(); // dotted baseline until the stream arrives
})();

// Mobile menu toggle
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
