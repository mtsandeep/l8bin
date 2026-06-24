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
  'architecture': '/docs/guides/architecture.md',
  'decisions': '/docs/guides/decisions.md',
  'failure-model': '/docs/guides/failure-model.md',
  'janitor': '/docs/guides/janitor.md',
  'user-flows': '/docs/guides/user-flows.md',
  'waker': '/docs/guides/waker.md',
};

let currentView = 'api';
let scalarApp = null;

// Reverse lookup: filename → DOC_MAP key (for rewriting .md links to SPA routes)
const DOC_KEY_BY_FILE = Object.fromEntries(
  Object.entries(DOC_MAP)
    .filter(([, url]) => url?.endsWith('.md'))
    .map(([key, url]) => [url.split('/').pop(), key])
);

// Init Scalar
function initScalar() {
  if (typeof Scalar === 'undefined') return;
  scalarApp = Scalar.createApiReference('#scalar-app', {
    url: '/docs/openapi.json',
    theme: 'purple',
    darkMode: true,
    layout: 'modern',
    showDeveloperTools: 'never',
    hiddenClients: true,
    hideTestRequestButton: true,
    customCss: `.scalar-app { --scalar-color-1: #7c3aed; --scalar-background: #030308; --scalar-sidebar-background: #030308; }`,
  });
}

// Switch view
async function showView(name, anchor) {
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
    history.replaceState(null, '', '#' + name + (anchor ? '/' + anchor : ''));
    await loadMarkdown(name, anchor);
  }

  // Close mobile sidebar
  document.getElementById('sidebar').classList.remove('mobile-open');
}

// Load and render markdown
async function loadMarkdown(name, anchor) {
  const url = DOC_MAP[name];
  if (!url) return;

  const content = document.getElementById('doc-content');
  content.innerHTML = '<div class="flex items-center gap-3 text-zinc-500"><div class="animate-spin h-5 w-5 border-2 border-violet-500/30 border-t-violet-500 rounded-full"></div>Loading...</div>';

  try {
    const resp = await fetch(url);
    if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
    // Vite SPA fallback can return index.html (200) for missing files — reject it
    const ct = resp.headers.get('content-type') || '';
    if (ct.includes('text/html')) throw new Error(`Document not found: ${url}`);
    const md = await resp.text();
    if (!md.trim() || md.trimStart().startsWith('<!')) throw new Error(`Document not found: ${url}`);
    content.innerHTML = renderMarkdown(md);
    // Scroll to anchor if provided, otherwise top of content
    if (anchor) {
      const target = document.getElementById(anchor);
      if (target) {
        target.scrollIntoView({ behavior: 'smooth', block: 'start' });
        return;
      }
    }
    content.scrollIntoView({ behavior: 'smooth', block: 'start' });
  } catch (e) {
    content.innerHTML = `<p class="text-red-400">Failed to load document: ${e.message}</p>`;
  }
}

// Simple markdown → HTML (no dependency, covers common patterns)
function renderMarkdown(md) {
  // Extract fenced code blocks first so later replacements (especially the
  // paragraph wrap) can't mangle their contents. Placeholders look like HTML
  // tags so the paragraph regex skips them.
  const codeBlocks = [];
  let html = md.replace(/\r\n/g, '\n')
    .replace(/```(\w*)\n([\s\S]*?)```/g, (_, lang, code) => {
      const escaped = escapeHtml(code.trimEnd());
      codeBlocks.push(`<pre class="code-block"><code class="lang-${lang || 'text'}">${escaped}</code></pre>`);
      return `<div data-cb="${codeBlocks.length - 1}"></div>`;
    });

  html = html
    // GFM tables: header row | separator | body rows
    .replace(/(^\|[^\n]+\|\s*\n\|[\s|:-]+\|\s*\n(?:\|[^\n]+\|\s*\n?)+)/gm, (block) => {
      const lines = block.trim().split('\n');
      const splitRow = (line) =>
        line.trim().replace(/^\|/, '').replace(/\|$/, '').split('|').map(c => c.trim());
      const header = splitRow(lines[0]);
      const rows = lines.slice(2).map(splitRow);
      let out = '<table class="md-table"><thead><tr>' +
        header.map(h => `<th>${h}</th>`).join('') +
        '</tr></thead><tbody>';
      for (const row of rows) out += '<tr>' + row.map(c => `<td>${c}</td>`).join('') + '</tr>';
      out += '</tbody></table>';
      return out + '\n';
    })
    // Inline code
    .replace(/`([^`]+)`/g, '<code class="inline-code">$1</code>')
    // Headers (with slug IDs for anchor navigation)
    .replace(/^#### (.+)$/gm, (_, t) => `<h4 id="${slugify(t)}">${t}</h4>`)
    .replace(/^### (.+)$/gm, (_, t) => `<h3 id="${slugify(t)}">${t}</h3>`)
    .replace(/^## (.+)$/gm, (_, t) => `<h2 id="${slugify(t)}">${t}</h2>`)
    .replace(/^# (.+)$/gm, (_, t) => `<h1 id="${slugify(t)}">${t}</h1>`)
    // Blockquotes
    .replace(/^> (.+)$/gm, '<blockquote>$1</blockquote>')
    // Bold & italic
    .replace(/\*\*\*(.+?)\*\*\*/g, '<strong><em>$1</em></strong>')
    .replace(/\*\*(.+?)\*\*/g, '<strong>$1</strong>')
    .replace(/\*(.+?)\*/g, '<em>$1</em>')
    // Links (in-page anchors and cross-doc links stay in SPA; external links open in new tab)
    .replace(/\[([^\]]+)\]\(([^)]+)\)/g, (_, text, href) => {
      if (href.startsWith('#')) return `<a href="${href}">${text}</a>`;
      // Cross-doc link: foo.md or foo.md#anchor → #foo or #foo/anchor
      const mdMatch = href.match(/^([^/]+\.md)(?:#(.+))?$/);
      if (mdMatch) {
        const key = DOC_KEY_BY_FILE[mdMatch[1]];
        if (key) {
          return mdMatch[2]
            ? `<a href="#${key}/${mdMatch[2]}">${text}</a>`
            : `<a href="#${key}">${text}</a>`;
        }
      }
      return `<a href="${href}" target="_blank" rel="noopener">${text}</a>`;
    })
    // Unordered lists
    .replace(/^[-*] (.+)$/gm, '<li class="ul-item">$1</li>')
    // Ordered lists
    .replace(/^\d+\. (.+)$/gm, '<li class="ol-item">$1</li>')
    // Horizontal rules
    .replace(/^---$/gm, '<hr />')
    // Paragraphs (lines not already wrapped in a tag)
    .replace(/^(?!<[a-z/])(.+)$/gm, '<p>$1</p>');

  // Wrap consecutive <li class="ul-item"> in <ul>, and <li class="ol-item"> in <ol>
  html = html.replace(/((?:<li class="ul-item">[\s\S]*?<\/li>\s*)+)/g, '<ul>$1</ul>');
  html = html.replace(/((?:<li class="ol-item">[\s\S]*?<\/li>\s*)+)/g, '<ol>$1</ol>');

  // Restore fenced code blocks
  html = html.replace(/<div data-cb="(\d+)"><\/div>/g, (_, i) => codeBlocks[Number(i)]);

  return html;
}

function escapeHtml(str) {
  return str.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
}

// Slugify header text for anchor IDs (matches clap-markdown's anchor convention)
function slugify(text) {
  return text
    .replace(/<[^>]+>/g, '') // strip any inline HTML (e.g. <code>)
    .toLowerCase()
    .replace(/[^\w\s-]/g, '') // drop non-word chars except spaces/hyphens
    .trim()
    .replace(/\s+/g, '-');
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
  const raw = location.hash.replace('#', '') || 'api';
  const [name, anchor] = raw.split('/');
  if (Object.hasOwn(DOC_MAP, name)) {
    showView(name, anchor);
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
