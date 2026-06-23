// LiteBin Docs — sidebar navigation, Scalar API viewer, markdown rendering

const DOC_MAP = {
  'api': null, // Scalar view
  'cli': '/docs/guides/cli-reference.md',
  'quickstart': '/quickstart.html', // dedicated page
  'multi-service': '/docs/guides/multi-service.md',
  'multi-server': '/docs/guides/multi-server.md',
  'volumes': '/docs/guides/volumes.md',
  'env-secrets': '/docs/guides/env-secrets.md',
  'configuration': '/docs/guides/configuration.md',
  'local-testing': '/docs/guides/local-testing.md',
  'security': '/docs/guides/security.md',
  'faq': '/docs/guides/faq.md',
};

let currentView = 'api';
let scalarApp = null;

// Init Scalar
function initScalar() {
  if (typeof Scalar === 'undefined') return;
  scalarApp = Scalar.createApiReference('#scalar-app', {
    url: '/docs/openapi.json',
    theme: 'purple',
    darkMode: true,
    layout: 'modern',
    customCss: `.scalar-app { --scalar-color-1: #7c3aed; --scalar-background: #030308; --scalar-sidebar-background: #030308; }`,
  });
}

// Switch view
async function showView(name) {
  const url = DOC_MAP[name];
  // External links (full page navigation)
  if (url && !url.endsWith('.md')) {
    window.location.href = url;
    return;
  }

  currentView = name;
  const apiView = document.getElementById('view-api');
  const docsView = document.getElementById('view-docs');

  // Update nav active state
  document.querySelectorAll('.nav-item').forEach(el => {
    el.classList.toggle('active', el.dataset.nav === name);
  });

  if (name === 'api') {
    apiView.classList.remove('hidden');
    docsView.classList.add('hidden');
    history.replaceState(null, '', '#api');
  } else {
    apiView.classList.add('hidden');
    docsView.classList.remove('hidden');
    history.replaceState(null, '', '#' + name);
    await loadMarkdown(name);
  }

  // Close mobile sidebar
  document.getElementById('sidebar').classList.remove('mobile-open');
}

// Load and render markdown
async function loadMarkdown(name) {
  const url = DOC_MAP[name];
  if (!url) return;

  const content = document.getElementById('doc-content');
  content.innerHTML = '<div class="flex items-center gap-3 text-zinc-500"><div class="animate-spin h-5 w-5 border-2 border-violet-500/30 border-t-violet-500 rounded-full"></div>Loading...</div>';

  try {
    const resp = await fetch(url);
    if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
    const md = await resp.text();
    content.innerHTML = renderMarkdown(md);
    // Scroll to top of content
    content.scrollIntoView({ behavior: 'smooth', block: 'start' });
  } catch (e) {
    content.innerHTML = `<p class="text-red-400">Failed to load document: ${e.message}</p>`;
  }
}

// Simple markdown → HTML (no dependency, covers common patterns)
function renderMarkdown(md) {
  let html = md
    // Code blocks (fenced)
    .replace(/```(\w*)\n([\s\S]*?)```/g, (_, lang, code) => {
      const escaped = escapeHtml(code.trimEnd());
      return `<pre class="code-block"><code class="lang-${lang || 'text'}">${escaped}</code></pre>`;
    })
    // Inline code
    .replace(/`([^`]+)`/g, '<code class="inline-code">$1</code>')
    // Headers
    .replace(/^#### (.+)$/gm, '<h4>$1</h4>')
    .replace(/^### (.+)$/gm, '<h3>$1</h3>')
    .replace(/^## (.+)$/gm, '<h2>$1</h2>')
    .replace(/^# (.+)$/gm, '<h1>$1</h1>')
    // Bold & italic
    .replace(/\*\*\*(.+?)\*\*\*/g, '<strong><em>$1</em></strong>')
    .replace(/\*\*(.+?)\*\*/g, '<strong>$1</strong>')
    .replace(/\*(.+?)\*/g, '<em>$1</em>')
    // Links
    .replace(/\[([^\]]+)\]\(([^)]+)\)/g, '<a href="$2" target="_blank" rel="noopener">$1</a>')
    // Unordered lists
    .replace(/^[-*] (.+)$/gm, '<li>$1</li>')
    // Horizontal rules
    .replace(/^---$/gm, '<hr />')
    // Paragraphs (lines not already wrapped)
    .replace(/^(?!<[a-z/])(.+)$/gm, '<p>$1</p>');

  // Wrap consecutive <li> in <ul>
  html = html.replace(/((?:<li>[\s\S]*?<\/li>\s*)+)/g, '<ul>$1</ul>');

  return html;
}

function escapeHtml(str) {
  return str.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
}

// Sidebar navigation click handler
document.getElementById('sidebar').addEventListener('click', (e) => {
  const link = e.target.closest('[data-nav]');
  if (!link) return;
  e.preventDefault();
  showView(link.dataset.nav);
});

// Back button
document.getElementById('doc-back').addEventListener('click', (e) => {
  e.preventDefault();
  showView('api');
});

// Mobile sidebar toggle
document.getElementById('sidebar-toggle').addEventListener('click', () => {
  document.getElementById('sidebar').classList.toggle('mobile-open');
});

// Hash routing
function handleHash() {
  const hash = location.hash.replace('#', '') || 'api';
  if (DOC_MAP.hasOwnProperty(hash)) {
    showView(hash);
  }
}

window.addEventListener('hashchange', handleHash);

// Boot
if (document.readyState === 'loading') {
  document.addEventListener('DOMContentLoaded', () => { initScalar(); handleHash(); });
} else {
  initScalar();
  handleHash();
}
