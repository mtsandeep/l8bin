import './style.css'

// ═══════ DOM REFERENCES ═══════
const connectionLabel = document.querySelector('.connection-label');

const cpuAvgVal = document.getElementById('cpu-avg-val');
const cpuBurstVal = document.getElementById('cpu-burst-val');
const cpuSteps = document.getElementById('cpu-steps');

const loadValSmall = document.getElementById('load-val-small');
const loadValHero = document.getElementById('load-val');
const load5 = document.getElementById('load-5');
const load15 = document.getElementById('load-15');

const ramValPerc = document.getElementById('ram-val-perc');
const ramVal = document.getElementById('ram-val');
const ramRingVal = document.getElementById('ram-ring-val');
const ramSteps = document.getElementById('ram-steps');
const swapValPerc = document.getElementById('swap-val-perc');
const swapVal = document.getElementById('swap-val');
const swapSteps = document.getElementById('swap-steps');
const swapBox = document.getElementById('swap-box');

const diskRingVal = document.getElementById('disk-ring-val');
const diskVal = document.getElementById('disk-val');
const diskUsedPercLarge = document.getElementById('disk-used-perc-large');
const diskSteps = document.getElementById('disk-steps');

const containerCount = document.getElementById('container-count');
const containersList = document.getElementById('containers-list');
const availableFilters = document.getElementById('available-filters');

const hostOs = document.getElementById('host-os');
const hostCpu = document.getElementById('host-cpu');
const hostUptime = document.getElementById('host-uptime');
const hdrContRun = document.getElementById('hdr-cont-run');
const hdrContSleep = document.getElementById('hdr-cont-sleep');
const hdrContTotal = document.getElementById('hdr-cont-total');
const statusDot = document.getElementById('status-dot');
const statusWrapper = document.getElementById('status-wrapper');

const monitorVer = document.getElementById('monitor-ver');
const monitorMem = document.getElementById('monitor-mem');
const monitorUptime = document.getElementById('monitor-uptime');

const dockerProcsSection = document.getElementById('docker-procs-section');
const dockerProcsToggle = document.getElementById('docker-procs-toggle');
const dockerProcsChevron = document.getElementById('docker-procs-chevron');
const dockerProcsDetail = document.getElementById('docker-procs-detail');
const dockerProcsCount = document.getElementById('docker-procs-count');
const dockerProcsRam = document.getElementById('docker-procs-ram');
const dockerProcsList = document.getElementById('docker-procs-list');

let lastData = null;
let activeStatusFilter = null;

// ═══════ FILTER LOGIC ═══════
window.setFilter = function (status) {
    activeStatusFilter = (activeStatusFilter === status) ? null : status;
    if (lastData && lastData.containers) renderContainers(lastData.containers);
};

// ═══════ UTILITIES ═══════
window.toggleDockerProcs = function() {
    if (!dockerProcsDetail) return;
    const isHidden = dockerProcsDetail.classList.contains('hidden');
    if (isHidden) {
        dockerProcsDetail.classList.remove('hidden');
        if (dockerProcsChevron) dockerProcsChevron.style.transform = 'rotate(90deg)';
        if (dockerProcsToggle) dockerProcsToggle.classList.add('rounded-b-none');
    } else {
        dockerProcsDetail.classList.add('hidden');
        if (dockerProcsChevron) dockerProcsChevron.style.transform = 'rotate(0deg)';
        if (dockerProcsToggle) dockerProcsToggle.classList.remove('rounded-b-none');
    }
};

function setConnectionStatus(status) {
    if (!connectionLabel || !statusDot || !statusWrapper) return;
    
    if (status === 'live') {
        connectionLabel.textContent = 'Live';
        connectionLabel.className = 'connection-label text-[0.65rem] font-bold uppercase tracking-widest transition-colors text-emerald-400';
        statusDot.className = 'w-2.5 h-2.5 rounded-full transition-colors animate-pulse bg-emerald-400 shadow-[0_0_10px_rgba(52,211,153,0.5)]';
        statusWrapper.className = 'flex pl-3 pr-4 py-2 rounded-none items-center gap-3 shrink-0 transition-colors bg-emerald-500/10 border border-emerald-500/20 shadow-[0_0_15px_rgba(52,211,153,0.05)] border-t-emerald-500/40';
    } else if (status === 'paused') {
        connectionLabel.textContent = 'Paused';
        connectionLabel.className = 'connection-label text-[0.65rem] font-bold uppercase tracking-widest transition-colors text-zinc-500';
        statusDot.className = 'w-2.5 h-2.5 rounded-full transition-colors bg-zinc-500';
        statusWrapper.className = 'flex pl-3 pr-4 py-2 rounded-none items-center gap-3 shrink-0 transition-colors bg-zinc-500/10 border border-zinc-500/20 shadow-[0_0_15px_rgba(113,113,122,0.05)] border-t-zinc-500/40';
    } else if (status === 'offline') {
        connectionLabel.textContent = 'Offline';
        connectionLabel.className = 'connection-label text-[0.65rem] font-bold uppercase tracking-widest transition-colors text-rose-500';
        statusDot.className = 'w-2.5 h-2.5 rounded-full transition-colors bg-rose-500 shadow-[0_0_10px_rgba(244,63,94,0.5)]';
        statusWrapper.className = 'flex pl-3 pr-4 py-2 rounded-none items-center gap-3 shrink-0 transition-colors bg-rose-500/10 border border-rose-500/20 shadow-[0_0_15px_rgba(244,63,94,0.05)] border-t-rose-500/40';
    } else {
        // Default to Connecting
        connectionLabel.textContent = 'Connecting...';
        connectionLabel.className = 'connection-label text-[0.65rem] font-bold uppercase tracking-widest transition-colors text-amber-400';
        statusDot.className = 'w-2.5 h-2.5 rounded-full transition-colors animate-pulse bg-amber-400 shadow-[0_0_10px_rgba(245,158,11,0.5)]';
        statusWrapper.className = 'flex pl-3 pr-4 py-2 rounded-none items-center gap-3 shrink-0 transition-colors bg-amber-500/10 border border-amber-500/20 shadow-[0_0_15px_rgba(245,158,11,0.05)] border-t-amber-500/40';
    }
}

function formatBytes(bytes, decimals = 1) {
    if (!+bytes) return '0 B';
    const k = 1024;
    const dm = decimals < 0 ? 0 : decimals;
    const sizes = ['B', 'KB', 'MB', 'GB', 'TB', 'PB'];
    const i = Math.floor(Math.log(bytes) / Math.log(k));
    return `${parseFloat((bytes / Math.pow(k, i)).toFixed(dm))} ${sizes[i]}`;
}

function formatUptime(seconds) {
    const d = Math.floor(seconds / (3600 * 24));
    const h = Math.floor(seconds % (3600 * 24) / 3600);
    const m = Math.floor(seconds % 3600 / 60);
    const parts = [];
    if (d > 0) parts.push(`${d}d`);
    parts.push(`${h}h`);
    parts.push(`${m}m`);
    return parts.join(' ');
}

function renderSteps(container, percent, count = 28) {
    if (!container) return;
    container.innerHTML = '';
    const activeCount = Math.round((percent / 100) * count);
    
    for (let i = 0; i < count; i++) {
        const step = document.createElement('div');
        step.className = 'step';
        if (i < activeCount) {
            step.classList.add('active');
            if (percent > 85) step.classList.add('danger');
            else if (percent > 75) step.classList.add('warning');
        }
        container.appendChild(step);
    }
}

function processContainerStatus(raw) {
    const s = raw.toLowerCase();
    if (s.includes('running') || s.includes('up')) return 'running';
    if (s.includes('exited')) return 'sleeping';
    return s;
}

// ═══════ UPDATE DASHBOARD ═══════
function updateDashboard(data) {
    if (!data) return;

    // Link status
    setConnectionStatus('live');

    // ── Host Info ──
    if (data.host) {
        if (hostOs) {
            const osVal = data.host.platform || data.host.os;
            hostOs.textContent = osVal;
            hostOs.title = osVal;
        }
        if (hostCpu && data.host.cpuModel) {
            const cpuVal = data.host.cpuModel.replace(/\(R\)|\(TM\)|\s+/g, ' ').trim();
            hostCpu.textContent = cpuVal;
            hostCpu.title = cpuVal;
        }
        if (hostUptime) hostUptime.textContent = formatUptime(data.host.uptime);
    }

    // ── System Stats ──
    if (data.system) {
        // CPU & Load
        const cpuAvg = data.system.cpu_avg || 0;
        const cpuInstant = data.system.cpu_burst || 0;
        const load = data.system.load || [0, 0, 0];
        
        if (loadValHero) loadValHero.textContent = load[0].toFixed(2);
        if (load5) load5.textContent = load[1].toFixed(2);
        if (load15) load15.textContent = load[2].toFixed(2);
        
        if (cpuAvgVal) cpuAvgVal.textContent = `${cpuAvg.toFixed(1)}%`;
        
        const roundedBurst = Math.round(cpuInstant);
        if (roundedBurst === 0) {
            window.burstZeroCount = (window.burstZeroCount || 0) + 1;
        } else {
            window.burstZeroCount = 0;
            window.lastNonZeroBurst = roundedBurst;
        }
        
        let displayBurst;
        if (roundedBurst === 0 && window.burstZeroCount < 3) {
            displayBurst = window.lastNonZeroBurst || 0;
        } else {
            displayBurst = roundedBurst;
        }
        
        if (cpuBurstVal) cpuBurstVal.textContent = `${displayBurst}%`;
        renderSteps(cpuSteps, cpuAvg, 30);

        // RAM
        const ramUsed = data.system.ram?.used || 0;
        const ramTotal = data.system.ram?.total || 1;
        const ramPerc = (ramUsed / ramTotal) * 100;
        if (ramValPerc) ramValPerc.textContent = `${Math.round(ramPerc)}%`;
        if (ramVal) ramVal.textContent = `${formatBytes(ramUsed)} / ${formatBytes(ramTotal, 0)}`;
        renderSteps(ramSteps, ramPerc, 30);

        // Swap (only show if total > 64MB)
        const swapTotal = data.system.swap?.total || 0;
        if (swapTotal > 64 * 1024 * 1024) {
            if (swapBox) swapBox.classList.remove('hidden');
            if (swapBox) swapBox.classList.add('flex');
            const swapUsed = data.system.swap?.used || 0;
            const swapPerc = (swapUsed / swapTotal) * 100;
            if (swapValPerc) swapValPerc.textContent = `${Math.round(swapPerc)}%`;
            if (swapVal) swapVal.textContent = `${formatBytes(swapUsed)} / ${formatBytes(swapTotal, 0)}`;
            renderSteps(swapSteps, swapPerc, 20);
        }

        // Disk
        const diskUsed = data.system.disk?.used || 0;
        const diskTotal = data.system.disk?.total || 1;
        const diskPerc = (diskUsed / diskTotal) * 100;
        if (diskUsedPercLarge) diskUsedPercLarge.textContent = `${Math.round(diskPerc)}%`;
        if (diskVal) diskVal.textContent = `${formatBytes(diskUsed)} / ${formatBytes(diskTotal, 0)}`;
        renderSteps(diskSteps, diskPerc, 20);
    }

    // ── Containers ──
    if (data.containers) {
        renderContainers(data.containers);
    }

    // ── Docker Daemon Processes ──
    if (data.docker_procs) {
        renderDockerProcesses(data.docker_procs);
    } else {
        if (dockerProcsSection) dockerProcsSection.classList.add('hidden');
    }

    // ── Monitor Self-Stats ──
    if (data.monitor) {
        if (monitorVer) monitorVer.textContent = data.monitor.version || 'vdev';
        if (monitorMem) monitorMem.textContent = `${data.monitor.memory_mb.toFixed(1)} MB`;
        if (monitorUptime) monitorUptime.textContent = formatUptime(data.monitor.uptime);
    }
}

function renderDockerProcesses(dp) {
    if (!dp || !dp.processes || dp.processes.length === 0) {
        if (dockerProcsSection) dockerProcsSection.classList.add('hidden');
        return;
    }
    if (dockerProcsSection) dockerProcsSection.classList.remove('hidden');

    if (dockerProcsCount) {
        dockerProcsCount.textContent = `${dp.processes.length} process${dp.processes.length !== 1 ? 'es' : ''}`;
    }
    if (dockerProcsRam) {
        dockerProcsRam.textContent = formatBytes(dp.total_ram);
    }

    // Only render list when expanded
    if (dockerProcsList && !dockerProcsDetail.classList.contains('hidden')) {
        dockerProcsList.innerHTML = (dp.groups || []).map(g => {
            const swapStr = g.swap > 0 ? formatBytes(g.swap) : '--';
            const badge = g.count > 1 ? `<span class="inline-flex items-center justify-center ml-2 px-1.5 py-0.5 bg-sky-500/15 border border-sky-500/30 text-sky-400 text-[0.55rem] font-mono font-bold leading-none">x${g.count}</span>` : '';
            return `<div class="container-row">
                <div class="col-span-7 flex items-center overflow-hidden gap-1">
                    <span class="c-name">${g.name}</span>${badge}
                </div>
                <div class="col-span-3 text-right">
                    <span class="c-metric-val">${formatBytes(g.ram)}</span>
                </div>
                <div class="col-span-2 text-right">
                    <span class="c-metric-val text-slate-500">${swapStr}</span>
                </div>
            </div>`;
        }).join('');
    }
}

function renderContainers(containers) {
    const totalCount = containers.length;
    const runningCount = containers.filter(c => processContainerStatus(c.status) === 'running').length;
    const sleepingCount = totalCount - runningCount;
    if (hdrContRun) hdrContRun.textContent = runningCount;
    if (hdrContSleep) hdrContSleep.textContent = sleepingCount;
    if (hdrContTotal) hdrContTotal.textContent = totalCount;

    if (availableFilters) {
        const counts = {};
        containers.forEach(c => {
            const s = processContainerStatus(c.status);
            counts[s] = (counts[s] || 0) + 1;
        });

        let filterHtml = '';
        if (activeStatusFilter) {
            filterHtml += `
                <div class="chip flex items-center gap-1 !bg-amber-500/20 !border-amber-500/40 !text-amber-300 hover:!bg-amber-500/30" title="Clear filter" onclick="window.setFilter('${activeStatusFilter}')">
                    ${activeStatusFilter}
                    <svg class="w-3 h-3 ml-1" fill="none" viewBox="0 0 24 24" stroke="currentColor"><path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M6 18L18 6M6 6l12 12" /></svg>
                </div>
            `;
        }
        
        filterHtml += Object.keys(counts).sort().map(s => {
            const isActive = activeStatusFilter === s;
            return `<div class="chip ${isActive ? 'active' : ''}" onclick="window.setFilter('${s}')">
                        ${s} (${counts[s]})
                    </div>`;
        }).join('');

        availableFilters.innerHTML = filterHtml;
    }

    let filtered = containers;
    if (activeStatusFilter) {
        filtered = containers.filter(c => processContainerStatus(c.status) === activeStatusFilter);
    }

    if (containersList) {
        containersList.innerHTML = '';
        if (filtered.length === 0) {
            containersList.innerHTML = `<div class="empty-state p-12 text-center text-slate-500 italic text-sm font-bold bg-black/20">No matching containers</div>`;
        } else {
            filtered.sort((a, b) => {
                const statusA = processContainerStatus(a.status);
                const statusB = processContainerStatus(b.status);
                if (statusA === 'running' && statusB !== 'running') return -1;
                if (statusA !== 'running' && statusB === 'running') return 1;
                return a.name.localeCompare(b.name);
            }).forEach(c => {
                const status = processContainerStatus(c.status);
                const row = document.createElement('div');
                row.className = 'container-row';
                
                row.innerHTML = `
                    <div class="col-span-1 sm:col-span-2 flex items-center">
                        <span class="status-pill ${status} !px-1.5 sm:!px-2.5">
                            <span class="status-dot"></span>
                            <span class="hidden sm:inline">${status}</span>
                        </span>
                    </div>
                    <div class="col-span-5 sm:col-span-4 flex items-center overflow-hidden gap-1">
                        <span class="c-name" title="${c.name}">${c.name}</span>
                    </div>
                    <div class="col-span-2 text-right">
                        <span class="c-metric-val">${status === 'running' ? `${(c.cpu || 0).toFixed(1)}%` : '—'}</span>
                    </div>
                    <div class="col-span-2 text-right">
                        <span class="c-metric-val">${status === 'running' ? formatBytes(c.memory || 0) : '—'}</span>
                    </div>
                    <div class="col-span-2 text-right">
                        <span class="c-metric-val">${formatBytes(c.disk || 0)}</span>
                    </div>
                `;
                containersList.appendChild(row);
            });
        }
    }
}

// ═══════ SSE CONNECTION ═══════
let currentEvtSource = null;
let tabHiddenTimer = null;
let tabVisibleTimer = null;

function connectStream() {
    if (currentEvtSource) return;
    setConnectionStatus('connecting');
    // Using relative URL due to nginx reverse proxy
    currentEvtSource = new EventSource("/stats/stream");

    currentEvtSource.onmessage = (event) => {
        try {
            const data = JSON.parse(event.data);
            lastData = data;
            updateDashboard(data);
        } catch (e) {
            console.error("Stream parse error:", e);
        }
    };

    currentEvtSource.onerror = () => {
        currentEvtSource.close();
        currentEvtSource = null;
        setConnectionStatus('offline');
        if (!document.hidden) {
            setTimeout(connectStream, 2000);
        }
    };
}

function disconnectStream() {
    if (currentEvtSource) {
        currentEvtSource.close();
        currentEvtSource = null;
    }
    setConnectionStatus('paused');
}

// ═══════ INIT ═══════
window.addEventListener('DOMContentLoaded', () => {
    connectStream();

    document.addEventListener('visibilitychange', () => {
        if (document.hidden) {
            clearTimeout(tabVisibleTimer);
            tabHiddenTimer = setTimeout(disconnectStream, 10000);
        } else {
            clearTimeout(tabHiddenTimer);
            tabVisibleTimer = setTimeout(connectStream, 2000);
        }
    });
});
