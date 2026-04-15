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

let lastData = null;
let activeStatusFilter = null;

// ═══════ FILTER LOGIC ═══════
window.setFilter = function (status) {
    activeStatusFilter = (activeStatusFilter === status) ? null : status;
    if (lastData && lastData.containers) renderContainers(lastData.containers);
};

// ═══════ UTILITIES ═══════
function setConnectionStatus(status) {
    if (!connectionLabel || !statusDot || !statusWrapper) return;
    
    if (status === 'live') {
        connectionLabel.textContent = 'Live';
        connectionLabel.className = 'connection-label text-[0.65rem] font-bold uppercase tracking-widest transition-colors text-emerald-400';
        statusDot.className = 'w-2.5 h-2.5 rounded-full transition-colors animate-pulse bg-emerald-400 shadow-[0_0_10px_rgba(52,211,153,0.5)]';
        statusWrapper.className = 'flex pl-3 pr-4 py-2 rounded-none items-center gap-3 shrink-0 transition-colors bg-emerald-500/10 border border-emerald-500/20 shadow-[0_0_15px_rgba(52,211,153,0.05)] border-t-emerald-500/40';
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
        if (hostOs) hostOs.textContent = data.host.platform || data.host.os;
        if (hostCpu && data.host.cpuModel) {
            hostCpu.textContent = data.host.cpuModel.replace(/\(R\)|\(TM\)|\s+/g, ' ').trim();
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

    // ── Monitor Self-Stats ──
    if (data.monitor) {
        if (monitorVer) monitorVer.textContent = data.monitor.version || 'vdev';
        if (monitorMem) monitorMem.textContent = `${data.monitor.memory_mb.toFixed(1)} MB`;
        if (monitorUptime) monitorUptime.textContent = formatUptime(data.monitor.uptime);
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
function connectStream() {
    setConnectionStatus('connecting');
    // Using relative URL due to Caddy reverse proxy
    const evtSource = new EventSource("/stream");

    evtSource.onmessage = (event) => {
        try {
            const data = JSON.parse(event.data);
            lastData = data;
            updateDashboard(data);
        } catch (e) {
            console.error("Stream parse error:", e);
        }
    };

    evtSource.onerror = () => {
        evtSource.close();
        setConnectionStatus('offline');
        setTimeout(connectStream, 2000);
    };
}

// ═══════ INIT ═══════
window.addEventListener('DOMContentLoaded', () => {
    connectStream();
});
