import {
  AlertTriangle,
  CheckCircle2,
  ChevronLeft,
  ChevronRight,
  Loader2,
  Moon,
  Rocket,
  Server,
  Shield,
  X,
} from 'lucide-react';
import { useEffect, useMemo, useState } from 'react';
import {
  DeployType,
  deployComposeProject,
  deployProject,
  fetchGlobalSettings,
  fetchNodes,
  type CapabilityInfo,
  type CompatibilityFinding,
  type Node,
  type ValidateComposeResponse,
  NodeStatus,
  validateCompose,
} from '../api';
import DeployProgressModal from './DeployProgressModal';
import ResourceLimitInput from './ResourceLimitInput';
import { useToast } from './ToastContext';

type DeployStep = 'details' | 'validate' | 'settings';

interface DeployFormProps {
  onDeploy: () => void;
  onClose: () => void;
  domain: string;
}

function catalogForId(catalog: CapabilityInfo[], id: string): CapabilityInfo | undefined {
  return catalog.find((c) => c.id === id);
}

function groupFindings(findings: CompatibilityFinding[]) {
  return {
    supported: findings.filter((f) => f.disposition === 'supported'),
    translated: findings.filter((f) => f.disposition === 'translated'),
    overridden: findings.filter((f) => f.disposition === 'overridden'),
    permissionRequired: findings.filter((f) => f.disposition === 'permission_required'),
    unsupported: findings.filter((f) => f.disposition === 'unsupported'),
  };
}

type FindingSections = {
  unsupported: string[];
  supported: string[];
  translated: string[];
  overridden: string[];
};

type ServiceFindings = {
  service: string | null;
  sections: FindingSections;
};

function emptyFindingSections(): FindingSections {
  return { unsupported: [], supported: [], translated: [], overridden: [] };
}

/** Group disposition sections under project/service cards. */
function findingsByService(findings: CompatibilityFinding[]): ServiceFindings[] {
  const project = emptyFindingSections();
  const services = new Map<string, FindingSections>();
  const supportedFields = new Map<string, string[]>();

  for (const finding of findings) {
    if (finding.disposition === 'permission_required') continue;

    const sections = finding.service
      ? (services.get(finding.service) ?? emptyFindingSections())
      : project;
    if (finding.service) services.set(finding.service, sections);

    if (finding.disposition === 'supported') {
      const field = /^([\w.-]+) is supported$/.exec(finding.message)?.[1];
      if (field && finding.service) {
        const fields = supportedFields.get(finding.service) ?? [];
        fields.push(field);
        supportedFields.set(finding.service, fields);
        continue;
      }
      sections.supported.push(finding.message);
    } else if (finding.disposition === 'unsupported') {
      const servicePrefix = finding.service ? `services.${finding.service}.` : '';
      const path = servicePrefix && finding.path.startsWith(servicePrefix)
        ? finding.path.slice(servicePrefix.length)
        : finding.path;
      sections.unsupported.push(`${path} — ${finding.message}`);
    } else {
      sections[finding.disposition].push(finding.message);
    }
  }

  for (const [service, fields] of supportedFields) {
    services.get(service)?.supported.unshift(fields.join(', '));
  }

  const cards: ServiceFindings[] = [];
  if (Object.values(project).some((lines) => lines.length > 0)) {
    cards.push({ service: null, sections: project });
  }
  for (const service of [...services.keys()].sort()) {
    cards.push({ service, sections: services.get(service)! });
  }
  return cards;
}

function messagesByService(findings: CompatibilityFinding[]) {
  const groups = new Map<string | null, string[]>();
  for (const finding of findings) {
    const lines = groups.get(finding.service) ?? [];
    lines.push(finding.message);
    groups.set(finding.service, lines);
  }
  return [...groups].map(([service, lines]) => ({ service, lines }));
}

const sectionStyles = {
  unsupported: { label: 'Unsupported', color: 'text-red-400', line: 'border-red-500/25 text-red-300/90' },
  supported: { label: 'Supported', color: 'text-emerald-400', line: 'border-emerald-500/25 text-emerald-300/80' },
  translated: { label: 'Adapted by LiteBin', color: 'text-sky-400', line: 'border-sky-500/25 text-sky-300/80' },
  overridden: { label: 'Overridden by LiteBin', color: 'text-slate-400', line: 'border-slate-600/40 text-slate-400' },
} as const;

function ServiceFindingCard({ card }: { card: ServiceFindings }) {
  return (
    <div className="rounded-md border border-slate-700/50 bg-slate-900/40 px-3 py-2.5">
      <div className="text-xs font-semibold text-slate-200 mb-2">
        {card.service ?? 'Project-wide'}
      </div>
      <div className="space-y-2.5">
        {(Object.keys(sectionStyles) as Array<keyof FindingSections>).map((key) => {
          const lines = card.sections[key];
          if (lines.length === 0) return null;
          const style = sectionStyles[key];
          return (
            <div key={key}>
              <div className={`text-[11px] font-medium ${style.color}`}>
                {style.label}
                <span className="ml-1 opacity-60 font-normal">({lines.length})</span>
              </div>
              <ul className="mt-1 space-y-1">
                {lines.map((line, index) => (
                  <li
                    key={index}
                    className={`text-[11px] leading-relaxed pl-2 border-l ${style.line}`}
                  >
                    {line}
                  </li>
                ))}
              </ul>
            </div>
          );
        })}
      </div>
    </div>
  );
}

export default function DeployForm({ onDeploy, onClose, domain: domainProp }: DeployFormProps) {
  const { showToast } = useToast();
  const [showProgress, setShowProgress] = useState(false);
  const [deployProjectId, setDeployProjectId] = useState('');
  const [step, setStep] = useState<DeployStep>('details');

  // Details step fields
  const [deployMode, setDeployMode] = useState<DeployType>(DeployType.Image);
  const [projectId, setProjectId] = useState('');
  const [projectName, setProjectName] = useState('');
  const [projectDescription, setProjectDescription] = useState('');
  const [image, setImage] = useState('');
  const [port, setPort] = useState('80');
  const [composeYaml, setComposeYaml] = useState('');

  // Validate step state
  const [validateResult, setValidateResult] = useState<ValidateComposeResponse | null>(null);
  const [validating, setValidating] = useState(false);
  const [approvedCapabilityIds, setApprovedCapabilityIds] = useState<string[]>([]);

  // Settings step fields — pre-populated with defaults
  const [autoStop, setAutoStop] = useState(true);
  const [timeoutMins, setTimeoutMins] = useState(15);
  const [autoStart, setAutoStart] = useState(true);
  const [cmd, setCmd] = useState('');
  const [memMb, setMemMb] = useState<number | null>(null); // null = use global default
  const [cpuLimit, setCpuLimit] = useState<number | null>(null);
  const [globalMemMb, setGlobalMemMb] = useState(256);
  const [globalCpu, setGlobalCpu] = useState(0.5);
  const [selectedNode, setSelectedNode] = useState<string | null>(null);
  const [domain, setDomain] = useState(domainProp);
  const [nodes, setNodes] = useState<Node[]>([]);

  // Fetch nodes + global settings when entering settings step
  useEffect(() => {
    if (step === 'settings') {
      if (nodes.length === 0)
        fetchNodes()
          .then(setNodes)
          .catch(() => {});
      fetchGlobalSettings()
        .then((s) => {
          setGlobalMemMb(s.default_memory_limit_mb);
          setGlobalCpu(s.default_cpu_limit);
          setDomain(s.domain);
          if (memMb === null) setMemMb(s.default_memory_limit_mb);
          if (cpuLimit === null) setCpuLimit(s.default_cpu_limit);
        })
        .catch(() => {});
    }
  }, [step, nodes.length, memMb, cpuLimit]);

  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const steps: DeployStep[] =
    deployMode === DeployType.Compose ? ['details', 'validate', 'settings'] : ['details', 'settings'];

  const stepLabels: Record<DeployStep, string> = {
    details: 'App details',
    validate: 'Validate',
    settings: 'Settings',
  };

  const detailsValid =
    projectId.trim() !== '' && (deployMode === DeployType.Image ? image.trim() !== '' : composeYaml.trim() !== '');
  const timeoutValid = timeoutMins >= 1;

  const findingGroups = useMemo(
    () => (validateResult ? groupFindings(validateResult.report.findings) : null),
    [validateResult],
  );

  const missingCaps = validateResult?.missing_capabilities ?? [];
  const hasUnsupported = (findingGroups?.unsupported.length ?? 0) > 0;
  const allMissingApproved =
    missingCaps.length === 0 || missingCaps.every((id) => approvedCapabilityIds.includes(id));
  const canProceedFromValidate =
    !!validateResult && validateResult.report.ok && !hasUnsupported && allMissingApproved;

  const runValidate = async () => {
    setError(null);
    setValidating(true);
    try {
      const result = await validateCompose(composeYaml.trim(), {
        project_id: projectId.trim() || undefined,
      });
      setValidateResult(result);
      // Keep approvals that are still missing; drop ones no longer required
      setApprovedCapabilityIds((prev) => prev.filter((id) => result.missing_capabilities.includes(id)));
      setStep('validate');
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Compose validation failed');
    } finally {
      setValidating(false);
    }
  };

  const handleDetailsSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!detailsValid) return;
    if (deployMode === DeployType.Image) {
      setStep('settings');
      return;
    }
    await runValidate();
  };

  const toggleApprovedCapability = (id: string) => {
    setApprovedCapabilityIds((prev) => (prev.includes(id) ? prev.filter((c) => c !== id) : [...prev, id]));
  };

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!timeoutValid) return;
    setError(null);
    setLoading(true);

    try {
      if (deployMode === DeployType.Compose) {
        const warnings = await deployComposeProject({
          project_id: projectId.trim(),
          compose: composeYaml.trim(),
          name: projectName.trim() || undefined,
          description: projectDescription.trim() || undefined,
          node_id: selectedNode,
          auto_stop_enabled: autoStop,
          auto_stop_timeout_mins: timeoutMins,
          auto_start_enabled: autoStart,
          grant_capabilities: approvedCapabilityIds.length > 0 ? approvedCapabilityIds : undefined,
        });
        for (const w of warnings) showToast(w, 'warning');
      } else {
        await deployProject({
          project_id: projectId.trim(),
          image: image.trim(),
          port: parseInt(port, 10),
          name: projectName.trim() || undefined,
          description: projectDescription.trim() || undefined,
          node_id: selectedNode,
          auto_stop_enabled: autoStop,
          auto_stop_timeout_mins: timeoutMins,
          auto_start_enabled: autoStart,
          cmd: cmd.trim() || undefined,
          memory_limit_mb: memMb,
          cpu_limit: cpuLimit,
        });
      }
      onDeploy();
      setDeployProjectId(projectId.trim());
      setShowProgress(true);
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Deploy failed');
    } finally {
      setLoading(false);
    }
  };

  const stepIndex = steps.indexOf(step);

  const StepIndicator = ({ label }: { label: string }) => (
    <div className="flex items-center gap-1">
      {steps.map((s, i) => (
        <div key={s} className="flex items-center gap-1">
          <div
            className={`w-6 h-6 rounded-full flex items-center justify-center text-[10px] font-medium ${
              step === s
                ? 'bg-violet-600 text-white'
                : stepIndex > i
                  ? 'bg-violet-900/50 text-violet-400'
                  : 'bg-slate-800 text-slate-500'
            }`}
          >
            {i + 1}
          </div>
          {i < steps.length - 1 && <ChevronRight size={12} className="text-slate-700" />}
        </div>
      ))}
      <span className="ml-2 text-xs text-slate-500">{label}</span>
    </div>
  );

  // Deploy progress modal (shown after deploy API returns)
  if (showProgress) {
    return (
      <DeployProgressModal
        projectId={deployProjectId}
        domain={domain}
        onClose={() => {
          setShowProgress(false);
          onClose();
        }}
      />
    );
  }

  const modalWidth = step === 'validate' ? 'max-w-2xl' : 'max-w-md';

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 backdrop-blur-sm">
      <div
        className={`bg-slate-800 border border-slate-700/50 rounded-lg w-full ${modalWidth} mx-4 shadow-2xl max-h-[85vh] flex flex-col`}
      >
        {/* Header */}
        <div className="flex items-center justify-between px-5 py-4 border-b border-slate-700/50 shrink-0">
          <h2 className="text-sm font-semibold text-slate-100">Deploy New App</h2>
          <button
            type="button"
            onClick={onClose}
            className="p-1 rounded-md text-slate-400 hover:text-slate-200 hover:bg-slate-700/50 transition-colors cursor-pointer"
          >
            <X size={16} />
          </button>
        </div>

        {/* Details step */}
        {step === 'details' && (
          <form onSubmit={handleDetailsSubmit} className="p-5 space-y-4 overflow-y-auto">
            <StepIndicator label={stepLabels.details} />

            {/* Deploy mode toggle */}
            <div className="flex items-center gap-2">
              <span className="text-xs text-slate-400">Docker Image</span>
              <button
                type="button"
                role="switch"
                aria-checked={deployMode === DeployType.Compose}
                onClick={() => {
                  setDeployMode((v) => (v === DeployType.Image ? DeployType.Compose : DeployType.Image));
                  setValidateResult(null);
                  setApprovedCapabilityIds([]);
                  setError(null);
                }}
                className={`relative inline-flex h-4 w-7 items-center rounded-full transition-colors cursor-pointer ${
                  deployMode === DeployType.Compose ? 'bg-violet-500' : 'bg-slate-600'
                }`}
              >
                <span
                  className={`inline-block h-3 w-3 transform rounded-full bg-white transition-transform ${
                    deployMode === DeployType.Compose ? 'translate-x-3.5' : 'translate-x-0.5'
                  }`}
                />
              </button>
              <span className="text-xs text-slate-400">Compose</span>
            </div>

            <div>
              <label htmlFor="deploy-project-id" className="block text-xs font-medium text-slate-400 mb-1.5">
                Project ID
              </label>
              <input
                id="deploy-project-id"
                type="text"
                value={projectId}
                onChange={(e) =>
                  setProjectId(
                    e.target.value
                      .toLowerCase()
                      .replace(/[^a-z0-9-]/g, '')
                      .slice(0, 63),
                  )
                }
                placeholder="my-app"
                required
                className="w-full px-3 py-2 bg-slate-900/50 border border-slate-700/50 rounded-md text-sm text-slate-200 placeholder:text-slate-600 focus:outline-none focus:border-violet-500/50 focus:ring-1 focus:ring-violet-500/25 transition-colors"
              />
              <p className="text-[11px] text-slate-500 mt-1">
                Used as subdomain:{' '}
                <span className="text-slate-400">
                  {projectId || 'my-app'}.{domain}
                </span>
              </p>
            </div>

            <div>
              <label htmlFor="deploy-display-name" className="block text-xs font-medium text-slate-400 mb-1.5">
                Display Name <span className="text-slate-600 font-normal">(optional)</span>
              </label>
              <input
                id="deploy-display-name"
                type="text"
                value={projectName}
                onChange={(e) => setProjectName(e.target.value)}
                placeholder="My App"
                className="w-full px-3 py-2 bg-slate-900/50 border border-slate-700/50 rounded-md text-sm text-slate-200 placeholder:text-slate-600 focus:outline-none focus:border-violet-500/50 focus:ring-1 focus:ring-violet-500/25 transition-colors"
              />
            </div>

            <div>
              <label htmlFor="deploy-description" className="block text-xs font-medium text-slate-400 mb-1.5">
                Description <span className="text-slate-600 font-normal">(optional)</span>
              </label>
              <input
                id="deploy-description"
                type="text"
                value={projectDescription}
                onChange={(e) => setProjectDescription(e.target.value)}
                placeholder="What this app does"
                className="w-full px-3 py-2 bg-slate-900/50 border border-slate-700/50 rounded-md text-sm text-slate-200 placeholder:text-slate-600 focus:outline-none focus:border-violet-500/50 focus:ring-1 focus:ring-violet-500/25 transition-colors"
              />
            </div>

            {deployMode === DeployType.Image && (
              <>
                <div>
                  <label htmlFor="deploy-docker-image" className="block text-xs font-medium text-slate-400 mb-1.5">
                    Docker Image
                  </label>
                  <input
                    id="deploy-docker-image"
                    type="text"
                    value={image}
                    onChange={(e) => setImage(e.target.value)}
                    placeholder="nginx:alpine"
                    required
                    className="w-full px-3 py-2 bg-slate-900/50 border border-slate-700/50 rounded-md text-sm text-slate-200 placeholder:text-slate-600 focus:outline-none focus:border-violet-500/50 focus:ring-1 focus:ring-violet-500/25 transition-colors"
                  />
                </div>

                <div>
                  <label htmlFor="deploy-app-port" className="block text-xs font-medium text-slate-400 mb-1.5">
                    App Port
                  </label>
                  <input
                    id="deploy-app-port"
                    type="number"
                    value={port}
                    onChange={(e) => setPort(e.target.value)}
                    placeholder="80"
                    required
                    min={1}
                    max={65535}
                    className="w-full px-3 py-2 bg-slate-900/50 border border-slate-700/50 rounded-md text-sm text-slate-200 placeholder:text-slate-600 focus:outline-none focus:border-violet-500/50 focus:ring-1 focus:ring-violet-500/25 transition-colors"
                  />
                  <p className="text-[11px] text-slate-500 mt-1">
                    Port your app listens on inside the container (e.g. 80 for nginx, 3000 for Node)
                  </p>
                </div>
              </>
            )}

            {deployMode === DeployType.Compose && (
              <div>
                <label htmlFor="deploy-docker-compose" className="block text-xs font-medium text-slate-400 mb-1.5">
                  Docker Compose
                </label>
                <textarea
                  id="deploy-docker-compose"
                  value={composeYaml}
                  onChange={(e) => setComposeYaml(e.target.value)}
                  placeholder={`services:\n  app:\n    image: nginx:alpine\n    ports:\n      - "80:80"`}
                  required
                  rows={8}
                  className="w-full px-3 py-2 bg-slate-900/50 border border-slate-700/50 rounded-md text-xs text-slate-200 font-mono placeholder:text-slate-600 focus:outline-none focus:border-violet-500/50 focus:ring-1 focus:ring-violet-500/25 transition-colors resize-none"
                />
                <p className="text-[11px] text-slate-500 mt-1">Paste your docker-compose YAML with prebuilt images</p>
              </div>
            )}

            {error && (
              <div className="px-3 py-2 rounded-md bg-red-500/10 border border-red-500/20 text-xs text-red-400">
                {error}
              </div>
            )}

            <button
              type="submit"
              disabled={!detailsValid || validating}
              className="w-full inline-flex items-center justify-center gap-2 px-4 py-2.5 rounded-md text-sm font-medium bg-violet-600 text-white hover:bg-violet-500 transition-colors disabled:opacity-50 disabled:cursor-not-allowed cursor-pointer"
            >
              {validating ? (
                <>
                  <Loader2 size={14} className="animate-spin" />
                  Validating...
                </>
              ) : deployMode === DeployType.Compose ? (
                <>
                  Parse and validate
                  <ChevronRight size={14} />
                </>
              ) : (
                <>
                  Next
                  <ChevronRight size={14} />
                </>
              )}
            </button>
          </form>
        )}

        {/* Validate step (compose only) */}
        {step === 'validate' && validateResult && findingGroups && (
          <div className="p-5 space-y-4 overflow-y-auto">
            <StepIndicator label={stepLabels.validate} />

            <div>
              <label htmlFor="validate-docker-compose" className="block text-xs font-medium text-slate-400 mb-1.5">
                Docker Compose
              </label>
              <textarea
                id="validate-docker-compose"
                value={composeYaml}
                onChange={(e) => setComposeYaml(e.target.value)}
                rows={6}
                className="w-full px-3 py-2 bg-slate-900/50 border border-slate-700/50 rounded-md text-xs text-slate-200 font-mono placeholder:text-slate-600 focus:outline-none focus:border-violet-500/50 focus:ring-1 focus:ring-violet-500/25 transition-colors resize-none"
              />
            </div>

            {validateResult.report.ok && !hasUnsupported && missingCaps.length === 0 && (
              <div className="flex items-center gap-2 px-3 py-2 rounded-md bg-emerald-500/10 border border-emerald-500/20 text-emerald-400">
                <CheckCircle2 size={14} />
                <span className="text-xs font-medium">All good — no extra capabilities required</span>
              </div>
            )}

            <div className="space-y-2">
              {missingCaps.length > 0 && !hasUnsupported && (
                <div className="px-3 py-3 rounded-md bg-amber-500/10 border border-amber-500/20 space-y-3">
                  <div className="flex items-center gap-2 text-amber-400">
                    <Shield size={14} />
                    <span className="text-xs font-medium">Capabilities requested</span>
                  </div>
                  <p className="text-[11px] text-amber-300/80">
                    Approve each capability below to continue. These will be granted on deploy.
                  </p>
                  <div className="space-y-2">
                    {missingCaps.map((id) => {
                      const info = catalogForId(validateResult.catalog, id);
                      const checked = approvedCapabilityIds.includes(id);
                      const reasonBuckets = messagesByService(
                        findingGroups.permissionRequired.filter((f) => f.capability === id),
                      );
                      return (
                        <label
                          key={id}
                          className="flex items-start gap-2.5 cursor-pointer rounded-md bg-slate-900/40 border border-amber-500/15 px-2.5 py-2"
                        >
                          <input
                            type="checkbox"
                            checked={checked}
                            onChange={() => toggleApprovedCapability(id)}
                            className="mt-0.5 rounded border-slate-600 bg-slate-800 text-violet-500 focus:ring-violet-500/40"
                          />
                          <div className="min-w-0">
                            <div className="text-xs text-slate-200 font-medium">{info?.label ?? id}</div>
                            {info?.description && (
                              <p className="text-[11px] text-slate-400 mt-0.5">{info.description}</p>
                            )}
                            {reasonBuckets.length > 0 && (
                              <div className="mt-1.5 space-y-1.5">
                                {reasonBuckets.map((bucket) => (
                                  <div key={bucket.service ?? 'project'}>
                                    {bucket.service && (
                                      <div className="text-[10px] font-medium text-amber-200/80">
                                        {bucket.service}
                                      </div>
                                    )}
                                    <ul className="space-y-0.5">
                                      {bucket.lines.map((r, i) => (
                                        <li key={i} className="text-[10px] text-amber-300/70">
                                          {r}
                                        </li>
                                      ))}
                                    </ul>
                                  </div>
                                ))}
                              </div>
                            )}
                            {info?.risk && (
                              <p className="text-[10px] text-amber-400/80 mt-1 flex items-center gap-1">
                                <AlertTriangle size={10} />
                                {info.risk}
                              </p>
                            )}
                          </div>
                        </label>
                      );
                    })}
                  </div>
                </div>
              )}

              {findingsByService(validateResult.report.findings).map((card) => (
                <ServiceFindingCard key={card.service ?? 'project'} card={card} />
              ))}
            </div>

            {error && (
              <div className="px-3 py-2 rounded-md bg-red-500/10 border border-red-500/20 text-xs text-red-400">
                {error}
              </div>
            )}

            <div className="flex gap-2">
              <button
                type="button"
                onClick={() => {
                  setError(null);
                  setStep('details');
                }}
                className="inline-flex items-center justify-center gap-1.5 px-4 py-2.5 rounded-md text-sm font-medium bg-slate-700 text-slate-300 hover:bg-slate-600 transition-colors cursor-pointer"
              >
                <ChevronLeft size={14} />
                Back
              </button>
              {hasUnsupported ? (
                <button
                  type="button"
                  onClick={runValidate}
                  disabled={validating || !composeYaml.trim()}
                  className="flex-1 inline-flex items-center justify-center gap-2 px-4 py-2.5 rounded-md text-sm font-medium bg-violet-600 text-white hover:bg-violet-500 transition-colors disabled:opacity-50 disabled:cursor-not-allowed cursor-pointer"
                >
                  {validating ? <Loader2 size={14} className="animate-spin" /> : null}
                  {validating ? 'Validating...' : 'Re-validate'}
                </button>
              ) : (
                <>
                  <button
                    type="button"
                    onClick={runValidate}
                    disabled={validating || !composeYaml.trim()}
                    className="inline-flex items-center justify-center gap-1.5 px-3 py-2.5 rounded-md text-sm font-medium bg-slate-700 text-slate-300 hover:bg-slate-600 transition-colors disabled:opacity-50 disabled:cursor-not-allowed cursor-pointer"
                  >
                    {validating ? <Loader2 size={14} className="animate-spin" /> : null}
                    Re-validate
                  </button>
                  <button
                    type="button"
                    onClick={() => setStep('settings')}
                    disabled={!canProceedFromValidate}
                    className="flex-1 inline-flex items-center justify-center gap-2 px-4 py-2.5 rounded-md text-sm font-medium bg-violet-600 text-white hover:bg-violet-500 transition-colors disabled:opacity-50 disabled:cursor-not-allowed cursor-pointer"
                  >
                    Next
                    <ChevronRight size={14} />
                  </button>
                </>
              )}
            </div>
          </div>
        )}

        {/* Settings step */}
        {step === 'settings' && (
          <form onSubmit={handleSubmit} className="p-5 space-y-4 overflow-y-auto">
            <StepIndicator label={stepLabels.settings} />

            <div className="flex items-center gap-1.5 text-slate-400 mb-1">
              <Moon size={13} />
              <span className="text-xs font-medium">Sleep Settings</span>
            </div>

            {/* Auto-stop toggle */}
            <label className="flex items-center justify-between gap-2 cursor-pointer">
              <span className="text-xs text-slate-300">Auto-stop when idle</span>
              <button
                type="button"
                role="switch"
                aria-checked={autoStop}
                onClick={() => setAutoStop((v) => !v)}
                className={`relative inline-flex h-4 w-7 items-center rounded-full transition-colors cursor-pointer ${
                  autoStop ? 'bg-violet-500' : 'bg-slate-600'
                }`}
              >
                <span
                  className={`inline-block h-3 w-3 transform rounded-full bg-white transition-transform ${
                    autoStop ? 'translate-x-3.5' : 'translate-x-0.5'
                  }`}
                />
              </button>
            </label>

            {/* Idle timeout — only shown when auto-stop is enabled */}
            {autoStop && (
              <div className="flex items-center justify-between gap-2">
                <span className="text-xs text-slate-300">Idle timeout (mins)</span>
                <input
                  type="number"
                  min={1}
                  value={timeoutMins}
                  onChange={(e) => setTimeoutMins(Number(e.target.value))}
                  className="w-16 bg-slate-900/50 border border-slate-700/50 rounded px-2 py-1 text-xs text-slate-200 text-right focus:outline-none focus:border-violet-500/50 focus:ring-1 focus:ring-violet-500/25 transition-colors"
                />
              </div>
            )}

            {/* Auto-start toggle */}
            <label className="flex items-center justify-between gap-2 cursor-pointer">
              <span className="text-xs text-slate-300">Auto-start on visit</span>
              <button
                type="button"
                role="switch"
                aria-checked={autoStart}
                onClick={() => setAutoStart((v) => !v)}
                className={`relative inline-flex h-4 w-7 items-center rounded-full transition-colors cursor-pointer ${
                  autoStart ? 'bg-violet-500' : 'bg-slate-600'
                }`}
              >
                <span
                  className={`inline-block h-3 w-3 transform rounded-full bg-white transition-transform ${
                    autoStart ? 'translate-x-3.5' : 'translate-x-0.5'
                  }`}
                />
              </button>
            </label>

            {/* Command override — only for single image mode */}
            {deployMode === DeployType.Image && (
              <div className="pt-1 border-t border-slate-700/50">
                <label htmlFor="deploy-cmd-override" className="block text-xs font-medium text-slate-400 mb-1.5">
                  Command override <span className="text-slate-600 font-normal">(optional)</span>
                </label>
                <input
                  id="deploy-cmd-override"
                  type="text"
                  value={cmd}
                  onChange={(e) => setCmd(e.target.value)}
                  placeholder="e.g. prefect server start --host 0.0.0.0"
                  className="w-full px-3 py-2 bg-slate-900/50 border border-slate-700/50 rounded-md text-xs text-slate-200 font-mono placeholder:text-slate-600 focus:outline-none focus:border-violet-500/50 focus:ring-1 focus:ring-violet-500/25 transition-colors"
                />
              </div>
            )}

            {/* Resource limits */}
            <div className="pt-1 border-t border-slate-700/50 space-y-3">
              <ResourceLimitInput
                label="Memory limit"
                value={memMb ?? globalMemMb}
                onChange={setMemMb}
                unit="MB"
                min={64}
                normalMax={4096}
                absoluteMax={65536}
                normalStep={64}
                overStep={1024}
                highLabel="high memory"
                minLabel="64 MB"
                normalMaxLabel="4 GB"
              />
              <ResourceLimitInput
                label="CPU limit"
                value={cpuLimit ?? globalCpu}
                onChange={setCpuLimit}
                unit="vCPU"
                min={0.1}
                normalMax={4}
                absoluteMax={32}
                normalStep={0.1}
                overStep={1}
                highLabel="high cpu"
                minLabel="0.1"
                normalMaxLabel="4 vCPU"
              />
            </div>

            {/* Node picker */}
            <div className="pt-1">
              <div className="flex items-center gap-1.5 text-slate-400 mb-2">
                <Server size={13} />
                <span className="text-xs font-medium">Agent Server</span>
              </div>
              <div className="space-y-1">
                <button
                  type="button"
                  onClick={() => setSelectedNode(null)}
                  className={`w-full flex items-center justify-between px-3 py-2 rounded-md border text-xs transition-colors cursor-pointer ${
                    selectedNode === null
                      ? 'border-violet-500/50 bg-violet-500/10 text-violet-300'
                      : 'border-slate-700/50 bg-slate-900/50 text-slate-400 hover:border-slate-600'
                  }`}
                >
                  <span>Automatic</span>
                  {selectedNode === null && <span className="text-[10px] text-violet-400">selected</span>}
                </button>
                {nodes
                  .filter((n) => n.status === NodeStatus.Online)
                  .map((node) => (
                    <button
                      key={node.id}
                      type="button"
                      onClick={() => setSelectedNode(node.id)}
                      className={`w-full flex items-center justify-between px-3 py-2 rounded-md border text-xs transition-colors cursor-pointer ${
                        selectedNode === node.id
                          ? 'border-violet-500/50 bg-violet-500/10 text-violet-300'
                          : 'border-slate-700/50 bg-slate-900/50 text-slate-400 hover:border-slate-600'
                      }`}
                    >
                      <span className="font-mono">
                        {node.name} <span className="text-slate-600">({node.id})</span>
                      </span>
                      {selectedNode === node.id && <span className="text-[10px] text-violet-400">selected</span>}
                    </button>
                  ))}
              </div>
            </div>

            {/* Validation message */}
            {autoStop && !timeoutValid && (
              <div className="px-3 py-2 rounded-md bg-amber-500/10 border border-amber-500/20 text-xs text-amber-400">
                Idle timeout must be at least 1 minute.
              </div>
            )}

            {error && (
              <div className="px-3 py-2 rounded-md bg-red-500/10 border border-red-500/20 text-xs text-red-400">
                {error}
              </div>
            )}

            <div className="flex gap-2">
              <button
                type="button"
                onClick={() => setStep(deployMode === DeployType.Compose ? 'validate' : 'details')}
                className="inline-flex items-center justify-center gap-1.5 px-4 py-2.5 rounded-md text-sm font-medium bg-slate-700 text-slate-300 hover:bg-slate-600 transition-colors cursor-pointer"
              >
                <ChevronLeft size={14} />
                Back
              </button>
              <button
                type="submit"
                disabled={loading || (autoStop && !timeoutValid)}
                className="flex-1 inline-flex items-center justify-center gap-2 px-4 py-2.5 rounded-md text-sm font-medium bg-violet-600 text-white hover:bg-violet-500 transition-colors disabled:opacity-50 disabled:cursor-not-allowed cursor-pointer"
              >
                {loading ? <Loader2 size={14} className="animate-spin" /> : <Rocket size={14} />}
                {loading ? 'Deploying...' : 'Deploy'}
              </button>
            </div>
          </form>
        )}
      </div>
    </div>
  );
}
