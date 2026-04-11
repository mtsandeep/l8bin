import './quickstart.css'

// Main tab switching
function switchMainTab(tabName) {
  document.querySelectorAll('.tab-content').forEach(el => el.classList.add('hidden'));
  document.getElementById('content-' + tabName).classList.remove('hidden');

  document.querySelectorAll('[id^="tab-"]').forEach(el => {
    el.classList.remove('tab-active');
    el.classList.add('tab-inactive');
  });
  document.getElementById('tab-' + tabName).classList.remove('tab-inactive');
  document.getElementById('tab-' + tabName).classList.add('tab-active');
}

window.switchMainTab = switchMainTab;

// Setup step switching
function showSetupStep(stepNum) {
  document.querySelectorAll('[id^="setup-step-"]').forEach(el => el.classList.add('hidden'));
  document.getElementById('setup-step-' + stepNum).classList.remove('hidden');

  const sidebar = document.getElementById('content-setup').querySelector('.lg\\:col-span-1');
  const steps = sidebar.querySelectorAll('.step-active, .step-inactive, .step-complete');

  steps.forEach((step, idx) => {
    step.classList.remove('step-active', 'step-inactive', 'step-complete');
    if (idx + 1 === stepNum) {
      step.classList.add('step-active');
    } else if (idx + 1 < stepNum) {
      step.classList.add('step-complete');
    } else {
      step.classList.add('step-inactive');
    }

    const num = step.querySelector('span:first-child');
    if (idx + 1 === stepNum) {
      num.className = 'w-8 h-8 rounded-full bg-violet-500/20 text-violet-400 flex items-center justify-center text-sm font-bold';
      num.textContent = idx + 1;
    } else if (idx + 1 < stepNum) {
      num.className = 'w-8 h-8 rounded-full bg-emerald-500/20 text-emerald-400 flex items-center justify-center text-sm font-bold';
      num.textContent = '✓';
    } else {
      num.className = 'w-8 h-8 rounded-full bg-zinc-800 text-zinc-400 flex items-center justify-center text-sm font-bold';
      num.textContent = idx + 1;
    }
  });
}

window.showSetupStep = showSetupStep;

// Deploy option switching
function showDeployOption(option) {
  document.querySelectorAll('.deploy-detail').forEach(el => el.classList.add('hidden'));
  document.getElementById('deploy-' + option).classList.remove('hidden');

  document.querySelectorAll('.deploy-option').forEach(el => {
    el.classList.remove('active');
    el.querySelector('.w-12').classList.remove('bg-violet-500/20');
    el.querySelector('.w-12').classList.add('bg-zinc-800');
    el.querySelector('svg').classList.remove('text-violet-400');
    el.querySelector('svg').classList.add('text-zinc-400');
  });

  const selected = document.getElementById('opt-' + option);
  selected.classList.add('active');
  selected.querySelector('.w-12').classList.remove('bg-zinc-800');
  selected.querySelector('.w-12').classList.add('bg-violet-500/20');
  selected.querySelector('svg').classList.remove('text-zinc-400');
  selected.querySelector('svg').classList.add('text-violet-400');
}

window.showDeployOption = showDeployOption;

// Path tab switching
function switchPathTab(os) {
  document.querySelectorAll('[id^="pcontent-"]').forEach(el => el.classList.add('hidden'));
  document.getElementById('pcontent-' + os).classList.remove('hidden');

  document.querySelectorAll('.ptab').forEach(el => {
    el.classList.remove('bg-white/8', 'text-white');
    el.classList.add('text-zinc-500');
  });
  const active = document.getElementById('ptab-' + os);
  active.classList.remove('text-zinc-500');
  active.classList.add('bg-white/8', 'text-white');
}

window.switchPathTab = switchPathTab;

// Copy code functionality
function copyCode(btn) {
  const code = btn.parentElement.querySelector('code, pre code')?.textContent ||
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
