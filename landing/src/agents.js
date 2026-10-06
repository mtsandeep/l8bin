// Scroll fade-in
const fadeObs = new IntersectionObserver(entries => {
  entries.forEach(e => { if (e.isIntersecting) { e.target.classList.add('visible'); fadeObs.unobserve(e.target); } });
}, { threshold: 0.1 });
document.querySelectorAll('.fade-in').forEach(el => fadeObs.observe(el));

// Copy code blocks (or the literal text in data-copy)
function copyCode(btn) {
  const code = btn.dataset.copy ||
               btn.parentElement.querySelector('code, pre code')?.textContent ||
               btn.parentElement.querySelector('code')?.textContent;
  if (code) {
    navigator.clipboard.writeText(code.trim());
    const originalHTML = btn.innerHTML;
    btn.innerHTML = '<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><polyline points="20 6 9 17 4 12"></polyline></svg>';
    btn.classList.add('text-emerald-400');
    setTimeout(() => {
      btn.innerHTML = originalHTML;
      btn.classList.remove('text-emerald-400');
    }, 3000);
  }
}

window.copyCode = copyCode;

// "Register it yourself" client tabs
document.querySelectorAll('[data-tabs]').forEach(scope => {
  scope.querySelectorAll('[data-tab]').forEach(btn => {
    btn.addEventListener('click', () => {
      scope.querySelectorAll('[data-tab]').forEach(b => b.classList.toggle('on', b === btn));
      scope.querySelectorAll('[data-panel]').forEach(p => p.classList.toggle('hidden', p.dataset.panel !== btn.dataset.tab));
    });
  });
});

// Rotating hero: ask/outcome pairs
const HERO_SLIDES = [
  { ask: '“Deploy it with LiteBin.”', ans: 'Consider it shipped — live at portfolio.l8b.in.' },
  { ask: '“Is it actually up?”', ans: 'Yes — it was asleep, woke on request, 200 OK.' },
  { ask: '“It crashed. Fix it.”', ans: 'Fixed — the DATABASE_URL was stale. Back up now.' },
  { ask: '“When did we last deploy?”', ans: '42 min ago — running smoothly since then.' },
];
const HERO_LABEL = '<span class="font-mono text-sm md:text-base text-zinc-500 mr-2">AI Agent:</span>';
const heroAsk = document.getElementById('hero-ask');
const heroAns = document.getElementById('hero-answer');
if (heroAsk && heroAns && !matchMedia('(prefers-reduced-motion: reduce)').matches) {
  let heroIdx = 0;
  setInterval(() => {
    heroAsk.classList.add('swap');
    heroAns.classList.add('swap');
    setTimeout(() => {
      heroIdx = (heroIdx + 1) % HERO_SLIDES.length;
      heroAsk.textContent = HERO_SLIDES[heroIdx].ask;
      heroAns.innerHTML = HERO_LABEL + HERO_SLIDES[heroIdx].ans;
      heroAsk.classList.remove('swap');
      heroAns.classList.remove('swap');
    }, 400);
  }, 4000);
}

// Claude Code-style session player: user prompt → thinking → tool calls with
// collapsed results → prose reply, one element at a time, looping.
const CC_SCRIPT = [
  { t: 'user', text: 'deploy my side project with litebin and make sure it\u2019s actually up' },
  { t: 'think', text: 'Reading l8b.toml\u2026' },
  { t: 'tool', name: 'deploy', args: 'project: "portfolio"', result: 'running \u00b7 https://portfolio.l8b.in', ms: 1500 },
  { t: 'tool', name: 'status', args: 'wait: true, healthy: true', result: 'http 200 \u00b7 342 ms', ms: 1200 },
  { t: 'reply', text: 'It\u2019s live and serving traffic at https://portfolio.l8b.in.' },
  { t: 'user', text: 'it crashed after I changed the DB url' },
  { t: 'think', text: 'Checking the logs\u2026' },
  { t: 'tool', name: 'logs', args: 'tail: 50', result: 'Error: ECONNREFUSED 5432', ms: 1000 },
  { t: 'tool', name: 'env_push', args: 'DATABASE_URL', result: 'applied \u00b7 recreating container', ms: 1400 },
  { t: 'reply', text: 'The DATABASE_URL was stale. It\u2019s pushed, and the app is back up and healthy.' },
];

const ccTerm = document.getElementById('cc-lines');
const ccSleep = (ms) => new Promise((r) => setTimeout(r, ms));

function ccLine(cls, html, first) {
  const d = document.createElement('div');
  d.className = 'cc-line ' + cls + (first ? '' : ' mt-2');
  d.innerHTML = html;
  ccTerm.appendChild(d);
  requestAnimationFrame(() => requestAnimationFrame(() => d.classList.add('on')));
  return d;
}

function ccBuild(step, first) {
  if (step.t === 'user') {
    return ccLine('cc-user' + (first ? '' : ' mt-6'), '<span class="cc-prompt">\u276F</span>' + step.text, true);
  }
  if (step.t === 'think') {
    return ccLine('cc-think cc-pad', '<span class="cc-star">\u273B</span>' + step.text);
  }
  if (step.t === 'tool') {
    const l = ccLine('cc-tool cc-pad', '<span class="cc-bullet">\u2B24</span><span class="cc-name">' + step.name + '</span><span class="cc-args">(' + step.args + ')</span>');
    const r = ccLine('cc-result', '<span class="cc-branch">\u23BF</span>' + step.result, true);
    return [l, r];
  }
  return ccLine('cc-reply cc-pad', '<span class="cc-bullet">\u2B24</span>' + step.text);
}

async function ccPlay() {
  while (true) {
    for (const step of CC_SCRIPT) {
      if (step.t === 'user') {
        await ccSleep(600);
        ccBuild(step, ccTerm.children.length === 0);
      } else if (step.t === 'think') {
        await ccSleep(450);
        const l = ccBuild(step);
        await ccSleep(1000);
        l.remove();
      } else if (step.t === 'tool') {
        await ccSleep(400);
        const parts = ccBuild(step);
        const bullet = parts[0].querySelector('.cc-bullet');
        bullet.innerHTML = '<span class="cc-spinner"></span>';
        parts[1].style.visibility = 'hidden';
        await ccSleep(step.ms);
        bullet.textContent = '\u2B24';
        parts[1].style.visibility = 'visible';
      } else if (step.t === 'reply') {
        await ccSleep(500);
        ccBuild(step);
        await ccSleep(1700);
      }
    }
    await ccSleep(4500);
    ccTerm.style.transition = 'opacity 0.4s';
    ccTerm.style.opacity = '0';
    await ccSleep(450);
    ccTerm.innerHTML = '';
    ccTerm.style.opacity = '1';
  }
}

function ccRenderStatic() {
  for (const step of CC_SCRIPT) ccBuild(step, ccTerm.children.length === 0);
  ccTerm.querySelectorAll('.cc-line').forEach((el) => el.classList.add('on'));
}

if (ccTerm) {
  const reduced = matchMedia('(prefers-reduced-motion: reduce)').matches;
  const start = () => (reduced ? ccRenderStatic() : ccPlay());
  const ccObs = new IntersectionObserver((entries) => {
    if (entries.some((e) => e.isIntersecting)) {
      ccObs.disconnect();
      start();
    }
  }, { threshold: 0.25 });
  ccObs.observe(ccTerm);
}
