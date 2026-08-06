import { Cpu, Network, Route, Shield, Tag, Waves, X } from 'lucide-react';
import { type ElementType, type ReactNode, useEffect, useState } from 'react';
import {
  type Node as ApiNode,
  DeployType,
  fetchProjectCapabilities,
  fetchProjectRoutes,
  formatBytes,
  type Project,
  type ProjectCapabilityStatus,
  type ProjectRoute,
  timeAgo,
} from '../../api';

interface ProjectDetailsModalProps {
  project: Project;
  nodes: ApiNode[];
  onClose: () => void;
}

function shortImage(image: string | null | undefined): string {
  if (!image) return '—';
  const hash = image.startsWith('sha256:') ? image.slice(7) : image;
  return hash.length > 24 ? `${hash.slice(0, 24)}…` : hash;
}

/** A labelled read-only value row. */
function Row({ label, value }: { label: string; value: ReactNode }) {
  return (
    <div className="flex items-start justify-between gap-3 py-1">
      <span className="text-[11px] text-slate-500 shrink-0 pt-0.5">{label}</span>
      <span className="text-[11px] text-slate-300 text-right min-w-0 break-all">{value ?? '—'}</span>
    </div>
  );
}

/** A titled grouping of rows. */
function Section({ icon: Icon, title, children }: { icon: ElementType; title: string; children: ReactNode }) {
  return (
    <div>
      <div className="flex items-center gap-1.5 mb-1 text-slate-400">
        <Icon size={12} />
        <span className="text-[10px] uppercase tracking-wider">{title}</span>
      </div>
      <div className="bg-slate-900/50 rounded-md px-3 py-1.5 divide-y divide-slate-800">{children}</div>
    </div>
  );
}

/** Read-only details for a project, sourced from the orchestrator DB (works when the node is offline). */
export default function ProjectDetailsModal({ project, nodes, onClose }: ProjectDetailsModalProps) {
  const [capabilities, setCapabilities] = useState<ProjectCapabilityStatus[]>([]);
  const [routes, setRoutes] = useState<ProjectRoute[]>([]);

  useEffect(() => {
    let cancelled = false;
    fetchProjectCapabilities(project.id)
      .then((caps) => !cancelled && setCapabilities(caps.filter((c) => c.granted)))
      .catch(() => {});
    fetchProjectRoutes(project.id)
      .then((r) => !cancelled && setRoutes(r))
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [project.id]);

  const node = project.node_id ? nodes.find((n) => n.id === project.node_id) : null;
  const primary = project.public_stats;
  const grantedCapabilities = capabilities.map((c) => c.label);

  return (
    <div className="fixed inset-0 z-50 flex items-start justify-center bg-black/60 backdrop-blur-sm pt-8">
      <div className="bg-slate-800 border border-slate-700/50 rounded-lg w-full max-w-lg mx-4 shadow-2xl max-h-[85vh] flex flex-col">
        {/* Header */}
        <div className="flex items-center justify-between px-5 py-4 border-b border-slate-700/50">
          <div className="flex items-center gap-2 min-w-0">
            <span className="text-sm font-semibold text-slate-100 truncate">{project.name || project.id}</span>
            <span className="text-[10px] px-1.5 py-0.5 rounded bg-amber-500/15 text-amber-400 shrink-0">offline</span>
          </div>
          <button
            type="button"
            onClick={onClose}
            className="text-slate-400 hover:text-slate-200 transition-colors p-1 cursor-pointer shrink-0"
          >
            <X size={16} />
          </button>
        </div>

        <div className="px-5 py-2 border-b border-slate-700/50">
          <p className="text-[11px] text-slate-500">
            Showing database state only — values may be stale while the node is unreachable.
          </p>
        </div>

        {/* Body */}
        <div className="overflow-y-auto px-5 py-4 space-y-3 flex-1">
          <Section icon={Tag} title="Overview">
            <Row label="Name" value={project.name || '—'} />
            {project.description ? <Row label="Description" value={project.description} /> : null}
            <Row label="Project ID" value={<span className="font-mono">{project.id}</span>} />
            <Row label="Type" value={project.deploy_type === DeployType.Compose ? 'Compose' : 'Image'} />
            <Row label="Workload" value={project.is_background ? 'Background' : 'Web app / HTTP API'} />
            <Row label="Last status" value={<span className="text-slate-500">(last known)</span>} />
            <Row label="Created" value={timeAgo(project.created_at)} />
            <Row label="Last active" value={timeAgo(project.last_active_at)} />
          </Section>

          <Section icon={Network} title="Node">
            <Row label="Name" value={node?.name ?? project.node_id ?? '—'} />
            <Row label="Node ID" value={<span className="font-mono">{project.node_id ?? '—'}</span>} />
            <Row label="Status" value={<span className="text-amber-400">offline</span>} />
          </Section>

          <Section icon={Tag} title="Workload">
            <Row label="Image" value={<span className="font-mono">{shortImage(primary?.image)}</span>} />
            {project.deploy_type === DeployType.Compose ? (
              <Row
                label="Services"
                value={
                  project.service_count != null
                    ? `${project.service_count}${project.service_summary ? ` (${project.service_summary})` : ''}`
                    : '—'
                }
              />
            ) : null}
            {primary?.cmd ? <Row label="Command" value={<span className="font-mono">{primary.cmd}</span>} /> : null}
            {!project.is_background && primary?.port ? (
              <Row label="Port" value={`${primary.mapped_port ?? '—'}:${primary.port}`} />
            ) : null}
          </Section>

          <Section icon={Cpu} title="Resources">
            <Row
              label="Memory limit"
              value={
                primary?.memory_limit_mb != null
                  ? formatBytes(primary.memory_limit_mb * 1024 * 1024)
                  : project.public_stats?.memory_limit_mb != null
                    ? formatBytes(project.public_stats.memory_limit_mb * 1024 * 1024)
                    : '—'
              }
            />
            <Row label="CPU limit" value={primary?.cpu_limit !== undefined ? `${primary.cpu_limit}` : '—'} />
          </Section>

          <Section icon={Network} title="Networking">
            <Row
              label="Domain"
              value={project.custom_domain ?? <span className="text-slate-500">(managed subdomain)</span>}
            />
            <Row label="Raw ports" value={project.allow_raw_ports ? 'enabled' : 'disabled'} />
            {routes.length > 0 ? (
              <Row
                label="Custom routes"
                value={
                  <span className="font-mono">{routes.map((r) => r.path ?? r.subdomain ?? r.upstream).join(', ')}</span>
                }
              />
            ) : null}
          </Section>

          <Section icon={Waves} title="Lifecycle">
            <Row
              label="Auto-stop"
              value={project.auto_stop_enabled ? `on · ${project.auto_stop_timeout_mins}m` : 'off'}
            />
            <Row label="Auto-start" value={project.auto_start_enabled ? 'on' : 'off'} />
          </Section>

          <Section icon={Shield} title="Capabilities">
            <Row label="Granted" value={grantedCapabilities.length > 0 ? grantedCapabilities.join(', ') : 'none'} />
          </Section>

          {primary?.volumes && primary.volumes.length > 0 ? (
            <Section icon={Route} title="Volumes">
              <Row
                label="Mounts"
                value={<span className="font-mono">{primary.volumes.map((v) => v.container_path).join(', ')}</span>}
              />
            </Section>
          ) : null}
        </div>

        {/* Footer */}
        <div className="flex justify-end px-5 py-3 border-t border-slate-700/50">
          <button
            type="button"
            onClick={onClose}
            className="px-4 py-2 rounded-md text-xs font-medium bg-slate-700 text-slate-300 hover:bg-slate-600 transition-colors cursor-pointer"
          >
            Close
          </button>
        </div>
      </div>
    </div>
  );
}
