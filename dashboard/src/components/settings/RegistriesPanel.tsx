import { ArrowDown, ArrowUp, Box, ChevronDown, ChevronRight, Key, Plus, Star, Trash2, X } from 'lucide-react';
import { useCallback, useEffect, useState } from 'react';
import {
  createRegistry,
  deleteRegistry,
  fetchRegistries,
  type MirrorInput,
  type RegistryInfo,
  type RegistryTestResult,
  testRegistry,
  updateRegistry,
} from '../../api';
import { useToast } from '../ToastContext';
import SettingsCard from './SettingsCard';

const GCR_MIRROR = 'https://mirror.gcr.io';

type TestState = { id: string; running: boolean; result: RegistryTestResult | null };
type MirrorRow = { url: string; user: string; pass: string; showAuth: boolean };

function testBadgeClass(result: RegistryTestResult): string {
  switch (result.status) {
    case 'ok':
      return 'bg-green-500/15 text-green-400';
    case 'unauthorized':
      return 'bg-amber-500/15 text-amber-400';
    default:
      return 'bg-red-500/15 text-red-400';
  }
}

export default function RegistriesPanel() {
  const [registries, setRegistries] = useState<RegistryInfo[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  // Create form state
  const [newName, setNewName] = useState('');
  const [newUrl, setNewUrl] = useState('');
  const [newUser, setNewUser] = useState('');
  const [newPass, setNewPass] = useState('');
  const [showNewAuth, setShowNewAuth] = useState(false);
  const [creating, setCreating] = useState(false);

  // Per-row state
  const [testing, setTesting] = useState<TestState>({ id: '', running: false, result: null });
  const [editingId, setEditingId] = useState<string | null>(null);
  const [editName, setEditName] = useState('');
  const [editUrl, setEditUrl] = useState('');
  const [editUser, setEditUser] = useState('');
  const [editPass, setEditPass] = useState('');
  const [editClearAuth, setEditClearAuth] = useState(false);
  const [showEditAuth, setShowEditAuth] = useState(false);
  const [editMirrors, setEditMirrors] = useState<MirrorRow[]>([]);
  const [saving, setSaving] = useState(false);
  const [deletingId, setDeletingId] = useState<string | null>(null);

  const { showToast } = useToast();
  const editingRegistry = registries.find((r) => r.id === editingId);

  const load = useCallback(() => {
    setLoading(true);
    fetchRegistries()
      .then(setRegistries)
      .catch(() => setError('Failed to load registries'))
      .finally(() => setLoading(false));
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  const handleCreate = async () => {
    if (!newName.trim() || !newUrl.trim()) {
      setError('Name and URL are required');
      return;
    }
    setCreating(true);
    setError(null);
    try {
      const credentials =
        newUser.trim() ? { username: newUser.trim(), password: newPass } : undefined;
      const created = await createRegistry(newName.trim(), newUrl.trim(), credentials);
      setRegistries((prev) => [...prev, created]);
      setNewName('');
      setNewUrl('');
      setNewUser('');
      setNewPass('');
      setShowNewAuth(false);
      showToast(`Registry "${created.name}" added`, 'success');
    } catch (e) {
      const msg = e instanceof Error ? e.message : 'Failed to add registry';
      setError(msg);
      showToast(msg);
    } finally {
      setCreating(false);
    }
  };

  const handleAddGcrMirror = () => {
    setEditMirrors((prev) =>
      prev.some((m) => m.url === GCR_MIRROR) ? prev : [...prev, { url: GCR_MIRROR, user: '', pass: '', showAuth: false }],
    );
  };

  const handleSetDefault = async (registry: RegistryInfo) => {
    try {
      const updated = await updateRegistry(registry.id, { is_default: true });
      // Exactly one default at a time — reflect that locally.
      setRegistries((prev) =>
        prev.map((r) => (r.id === updated.id ? updated : updated.is_default ? { ...r, is_default: false } : r)),
      );
      showToast(`"${updated.name}" is now the default registry`, 'success');
    } catch (e) {
      showToast(e instanceof Error ? e.message : 'Failed to set default');
    }
  };

  const handleTest = async (registry: RegistryInfo) => {
    setTesting({ id: registry.id, running: true, result: null });
    try {
      const result = await testRegistry(registry.id);
      setTesting({ id: registry.id, running: false, result });
      if (result.status !== 'ok') showToast(result.message);
    } catch (e) {
      const msg = e instanceof Error ? e.message : 'Test failed';
      setTesting({ id: registry.id, running: false, result: { status: 'unreachable', message: msg } });
      showToast(msg);
    }
  };

  const startEdit = (registry: RegistryInfo) => {
    setEditingId(registry.id);
    setEditName(registry.name);
    setEditUrl(registry.url);
    setEditUser('');
    setEditPass('');
    setEditClearAuth(false);
    setShowEditAuth(false);
    setEditMirrors(registry.mirrors.map((m) => ({ url: m.url, user: '', pass: '', showAuth: false })));
  };

  const handleSave = async (id: string) => {
    setSaving(true);
    try {
      const mirrors: MirrorInput[] = editMirrors
        .filter((m) => m.url.trim())
        .map((m) => ({
          url: m.url.trim(),
          username: m.user.trim() || undefined,
          password: m.user.trim() ? m.pass : undefined,
        }));
      const updated = await updateRegistry(id, {
        name: editName.trim() || undefined,
        url: editUrl.trim() || undefined,
        credentials: editUser.trim() ? { username: editUser.trim(), password: editPass } : undefined,
        clear_auth: editClearAuth || undefined,
        mirrors,
      });
      setRegistries((prev) => prev.map((r) => (r.id === id ? updated : r)));
      setEditingId(null);
      showToast('Registry updated', 'success');
    } catch (e) {
      showToast(e instanceof Error ? e.message : 'Failed to update registry');
    } finally {
      setSaving(false);
    }
  };

  const handleDelete = async (id: string) => {
    setDeletingId(id);
    try {
      await deleteRegistry(id);
      setRegistries((prev) => prev.filter((r) => r.id !== id));
      showToast('Registry removed', 'success');
    } catch (e) {
      showToast(e instanceof Error ? e.message : 'Failed to delete registry');
    } finally {
      setDeletingId(null);
    }
  };

  const moveMirror = (index: number, delta: number) => {
    setEditMirrors((prev) => {
      const next = [...prev];
      const target = index + delta;
      if (target < 0 || target >= next.length) return prev;
      [next[index], next[target]] = [next[target], next[index]];
      return next;
    });
  };

  const updateMirror = (index: number, patch: Partial<MirrorRow>) => {
    setEditMirrors((prev) => prev.map((m, i) => (i === index ? { ...m, ...patch } : m)));
  };

  const credentialsToggle = (open: boolean, onClick: () => void, label: string) => (
    <button
      type="button"
      onClick={onClick}
      className="flex items-center gap-1.5 text-xs text-slate-400 hover:text-slate-200 transition-colors cursor-pointer"
    >
      <Key size={12} />
      {label}
      {open ? <ChevronDown size={14} className="text-slate-500" /> : <ChevronRight size={14} className="text-slate-500" />}
    </button>
  );

  const credentialFields = (
    user: string,
    onUser: (value: string) => void,
    pass: string,
    onPass: (value: string) => void,
    idPrefix: string,
  ) => (
    <div className="grid grid-cols-1 sm:grid-cols-2 gap-2">
      <div>
        <label htmlFor={`${idPrefix}-user`} className="block text-xs text-slate-400 mb-1">
          Username
        </label>
        <input
          id={`${idPrefix}-user`}
          type="text"
          autoComplete="off"
          value={user}
          onChange={(e) => onUser(e.target.value)}
          placeholder="user"
          className="w-full bg-slate-900 border border-slate-700/50 rounded-md px-3 py-2 text-xs text-slate-200 placeholder:text-slate-600 focus:outline-none focus:border-violet-500"
        />
      </div>
      <div>
        <label htmlFor={`${idPrefix}-pass`} className="block text-xs text-slate-400 mb-1">
          Password / token
        </label>
        <input
          id={`${idPrefix}-pass`}
          type="password"
          autoComplete="new-password"
          value={pass}
          onChange={(e) => onPass(e.target.value)}
          placeholder="PAT, access token, password…"
          className="w-full bg-slate-900 border border-slate-700/50 rounded-md px-3 py-2 text-xs text-slate-200 placeholder:text-slate-600 focus:outline-none focus:border-violet-500"
        />
      </div>
    </div>
  );

  return (
    <div className="space-y-5">
      {error && (
        <div className="text-xs text-red-400 bg-red-500/10 border border-red-500/20 rounded px-3 py-2">{error}</div>
      )}

      <SettingsCard
        icon={Plus}
        title="Add registry"
        description="Credentials optional; mirrors can be added after creation."
      >
        <div className="space-y-3">
          <div className="grid grid-cols-1 sm:grid-cols-2 gap-3">
            <div>
              <label htmlFor="registry-name" className="block text-xs text-slate-400 mb-1">
                Name
              </label>
              <input
                id="registry-name"
                type="text"
                placeholder="My Harbor, company registry…"
                value={newName}
                onChange={(e) => setNewName(e.target.value)}
                className="w-full bg-slate-900 border border-slate-700/50 rounded-md px-3 py-2 text-xs text-slate-200 placeholder:text-slate-600 focus:outline-none focus:border-violet-500"
              />
            </div>
            <div>
              <label htmlFor="registry-url" className="block text-xs text-slate-400 mb-1">
                URL
              </label>
              <input
                id="registry-url"
                type="text"
                placeholder="https://registry.example.com"
                value={newUrl}
                onChange={(e) => setNewUrl(e.target.value)}
                className="w-full bg-slate-900 border border-slate-700/50 rounded-md px-3 py-2 text-xs text-slate-200 placeholder:text-slate-600 focus:outline-none focus:border-violet-500"
              />
            </div>
          </div>
          {credentialsToggle(showNewAuth, () => setShowNewAuth((v) => !v), 'Add Credentials')}
          {showNewAuth && credentialFields(newUser, setNewUser, newPass, setNewPass, 'new-registry')}
          <button
            type="button"
            onClick={handleCreate}
            disabled={creating}
            className="w-full py-2 rounded-md text-xs font-medium bg-violet-600 text-white hover:bg-violet-500 transition-colors disabled:opacity-50 cursor-pointer"
          >
            {creating ? 'Adding...' : 'Add Registry'}
          </button>
        </div>
      </SettingsCard>

      <SettingsCard
        icon={Box}
        title={`Registries${registries.length > 0 ? ` (${registries.length})` : ''}`}
        description="Registries, their mirrors, and the default for bare image names."
      >
        {loading ? (
          <div className="flex justify-center py-4">
            <div className="w-4 h-4 border-2 border-slate-700 border-t-violet-500 rounded-full animate-spin" />
          </div>
        ) : registries.length === 0 ? (
          <p className="text-xs text-slate-500 py-3 text-center">No registries configured</p>
        ) : (
          <div className="space-y-1.5">
            {registries.map((registry) => (
              <div
                key={registry.id}
                className={`rounded-md px-3 py-2 border border-slate-700/30 ${
                  registry.is_builtin ? 'bg-slate-800/40' : 'bg-slate-900/50'
                }`}
              >
                <div className="flex items-center justify-between gap-2">
                  <div className="min-w-0 flex-1">
                    <div className="flex items-center gap-1.5 flex-wrap">
                      <span className="text-xs text-slate-200 truncate">{registry.name}</span>
                      {registry.is_default && (
                        <span className="shrink-0 px-1.5 py-0.5 rounded text-xs font-medium bg-violet-500/15 text-violet-300">
                          default
                        </span>
                      )}
                      {registry.mirrors.length > 0 && (
                        <span
                          className="shrink-0 px-1.5 py-0.5 rounded text-xs font-medium bg-cyan-500/15 text-cyan-300"
                          title={registry.mirrors.map((m) => `${m.url}${m.has_auth ? ' (credentials)' : ''}`).join(', ')}
                        >
                          {registry.mirrors.length} mirror{registry.mirrors.length > 1 ? 's' : ''}
                        </span>
                      )}
                      <span
                        className={`shrink-0 px-1.5 py-0.5 rounded text-xs font-medium ${
                          registry.has_auth ? 'bg-green-500/15 text-green-400' : 'bg-slate-700/50 text-slate-400'
                        }`}
                        title={registry.has_auth ? `Credentials ending …${registry.auth_hint ?? ''}` : 'Anonymous pulls'}
                      >
                        {registry.has_auth ? `credentials …${registry.auth_hint ?? ''}` : 'anonymous'}
                      </span>
                    </div>
                    <div className="text-xs text-slate-500 font-mono truncate">{registry.url}</div>
                  </div>
                  <div className="flex items-center gap-1 shrink-0">
                    <button
                      type="button"
                      onClick={() => !registry.is_default && handleSetDefault(registry)}
                      disabled={registry.is_default}
                      className={
                        registry.is_default
                          ? 'p-1 text-violet-300 cursor-default'
                          : 'p-1 rounded text-slate-500 hover:text-violet-300 hover:bg-slate-700/50 transition-colors cursor-pointer'
                      }
                      title={
                        registry.is_default
                          ? 'Default registry — click another registry’s star to switch'
                          : 'Make the default for image names without a registry host'
                      }
                    >
                      <Star size={13} fill={registry.is_default ? 'currentColor' : 'none'} />
                    </button>
                    <button
                      type="button"
                      onClick={() => handleTest(registry)}
                      disabled={testing.running && testing.id === registry.id}
                      className="px-2 py-1 rounded text-xs text-slate-400 hover:text-slate-200 hover:bg-slate-700/50 transition-colors cursor-pointer disabled:opacity-50"
                      title="Test connectivity and credentials"
                    >
                      {testing.running && testing.id === registry.id ? 'Testing…' : 'Test'}
                    </button>
                    <button
                      type="button"
                      onClick={() => (editingId === registry.id ? setEditingId(null) : startEdit(registry))}
                      className="px-2 py-1 rounded text-xs text-slate-400 hover:text-slate-200 hover:bg-slate-700/50 transition-colors cursor-pointer"
                      title="Edit registry"
                    >
                      {editingId === registry.id ? 'Close' : 'Edit'}
                    </button>
                    {!registry.is_builtin ? (
                      <button
                        type="button"
                        onClick={() => handleDelete(registry.id)}
                        disabled={deletingId === registry.id}
                        className="p-1 rounded text-slate-500 hover:text-red-400 hover:bg-red-500/10 transition-colors cursor-pointer disabled:opacity-50"
                        title="Delete registry"
                      >
                        <Trash2 size={13} />
                      </button>
                    ) : (
                      <button
                        type="button"
                        disabled
                        className="p-1 rounded text-slate-600 cursor-not-allowed"
                        title="Built-in registries cannot be deleted"
                      >
                        <Trash2 size={13} />
                      </button>
                    )}
                  </div>
                </div>

                {testing.id === registry.id && testing.result && (
                  <div className={`mt-2 px-2 py-1.5 rounded text-xs inline-block ${testBadgeClass(testing.result)}`}>
                    {testing.result.message}
                  </div>
                )}

                {editingId === registry.id && (
                  <div className="mt-3 space-y-3 border-t border-slate-700/30 pt-3">
                    <div className="grid grid-cols-1 sm:grid-cols-2 gap-3">
                      <div>
                        <label className="block text-xs text-slate-400 mb-1">Name</label>
                        <input
                          type="text"
                          value={editName}
                          onChange={(e) => setEditName(e.target.value)}
                          className="w-full bg-slate-900 border border-slate-700/50 rounded-md px-3 py-2 text-xs text-slate-200 placeholder:text-slate-600 focus:outline-none focus:border-violet-500"
                        />
                      </div>
                      <div>
                        <label className="block text-xs text-slate-400 mb-1">URL</label>
                        <input
                          type="text"
                          value={editUrl}
                          onChange={(e) => setEditUrl(e.target.value)}
                          placeholder="https://registry.example.com"
                          className="w-full bg-slate-900 border border-slate-700/50 rounded-md px-3 py-2 text-xs text-slate-200 placeholder:text-slate-600 focus:outline-none focus:border-violet-500"
                        />
                      </div>
                    </div>

                    {credentialsToggle(
                      showEditAuth,
                      () => setShowEditAuth((v) => !v),
                      registry.has_auth ? `Credentials (…${registry.auth_hint ?? ''})` : 'Add Credentials',
                    )}
                    {showEditAuth && (
                      <div className="space-y-1.5">
                        {credentialFields(editUser, setEditUser, editPass, setEditPass, 'edit-registry')}
                        {registry.has_auth && (
                          <label className="flex items-center gap-2 cursor-pointer">
                            <input
                              type="checkbox"
                              checked={editClearAuth}
                              onChange={(e) => setEditClearAuth(e.target.checked)}
                              className="accent-violet-500"
                            />
                            <span className="text-xs text-slate-400">Remove stored credentials (pull anonymously)</span>
                          </label>
                        )}
                      </div>
                    )}

                    <div>
                      <div className="flex items-center justify-between mb-2">
                        <p className="text-xs text-slate-400">Mirrors (tried in order)</p>
                        {registry.id === 'docker-hub' && !editMirrors.some((m) => m.url === GCR_MIRROR) && (
                          <button
                            type="button"
                            onClick={handleAddGcrMirror}
                            className="text-xs text-cyan-300/80 hover:text-cyan-300 underline decoration-dotted underline-offset-2 cursor-pointer"
                            title="Add mirror.gcr.io — Google's Docker Hub mirror; keeps Hub pulls off the rate limit"
                          >
                            Add Google's mirror
                          </button>
                        )}
                      </div>
                      <div className="space-y-2">
                        {editMirrors.map((mirror, index) => {
                          const stored = editingRegistry?.mirrors[index];
                          return (
                            <div key={index} className="rounded-md border border-slate-700/40 p-2.5 space-y-2">
                              <div className="flex items-center gap-1">
                                <div className="flex-1">
                                  <label className="block text-xs text-slate-500 mb-1">URL</label>
                                  <input
                                    type="text"
                                    value={mirror.url}
                                    onChange={(e) => updateMirror(index, { url: e.target.value })}
                                    placeholder="https://mirror.example.com"
                                    className="w-full bg-slate-900 border border-slate-700/50 rounded-md px-3 py-2 text-xs text-slate-200 placeholder:text-slate-600 focus:outline-none focus:border-violet-500 font-mono"
                                  />
                                </div>
                                <button
                                  type="button"
                                  onClick={() => moveMirror(index, -1)}
                                  disabled={index === 0}
                                  className="mt-5 p-1.5 rounded text-slate-500 hover:text-slate-200 hover:bg-slate-700/50 transition-colors cursor-pointer disabled:opacity-30"
                                  title="Try before the mirror above"
                                >
                                  <ArrowUp size={12} />
                                </button>
                                <button
                                  type="button"
                                  onClick={() => moveMirror(index, 1)}
                                  disabled={index === editMirrors.length - 1}
                                  className="mt-5 p-1.5 rounded text-slate-500 hover:text-slate-200 hover:bg-slate-700/50 transition-colors cursor-pointer disabled:opacity-30"
                                  title="Try after the mirror below"
                                >
                                  <ArrowDown size={12} />
                                </button>
                                <button
                                  type="button"
                                  onClick={() => setEditMirrors((prev) => prev.filter((_, i) => i !== index))}
                                  className="mt-5 p-1.5 rounded text-slate-500 hover:text-red-400 hover:bg-red-500/10 transition-colors cursor-pointer"
                                  title="Remove mirror"
                                >
                                  <X size={12} />
                                </button>
                              </div>
                              {credentialsToggle(
                                mirror.showAuth,
                                () => updateMirror(index, { showAuth: !mirror.showAuth }),
                                stored?.has_auth ? `Credentials (…${stored.auth_hint ?? ''})` : 'Add Credentials',
                              )}
                              {mirror.showAuth &&
                                credentialFields(
                                  mirror.user,
                                  (value) => updateMirror(index, { user: value }),
                                  mirror.pass,
                                  (value) => updateMirror(index, { pass: value }),
                                  `mirror-${index}`,
                                )}
                            </div>
                          );
                        })}
                        <button
                          type="button"
                          onClick={() => setEditMirrors((prev) => [...prev, { url: '', user: '', pass: '', showAuth: false }])}
                          className="px-2 py-1 rounded text-xs text-slate-400 hover:text-slate-200 hover:bg-slate-700/50 transition-colors cursor-pointer"
                        >
                          + Add mirror
                        </button>
                      </div>
                    </div>

                    <div className="flex gap-2 pt-1">
                      <button
                        type="button"
                        onClick={() => handleSave(registry.id)}
                        disabled={saving}
                        className="px-3 py-1.5 rounded-md text-xs font-medium bg-violet-600 text-white hover:bg-violet-500 transition-colors disabled:opacity-50 cursor-pointer"
                      >
                        {saving ? 'Saving…' : 'Save'}
                      </button>
                      <button
                        type="button"
                        onClick={() => setEditingId(null)}
                        className="px-3 py-1.5 rounded-md text-xs font-medium bg-slate-800 text-slate-300 hover:bg-slate-700 transition-colors cursor-pointer"
                      >
                        Cancel
                      </button>
                    </div>
                  </div>
                )}
              </div>
            ))}
          </div>
        )}
      </SettingsCard>
    </div>
  );
}
