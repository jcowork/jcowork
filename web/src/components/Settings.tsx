import { useState, useEffect } from 'react';

interface ModelInfo {
  id: string;
  name: string;
  context_length: number;
}

interface ProviderInfo {
  id: string;
  name: string;
  models: ModelInfo[];
}

interface SettingsProps {
  onClose: () => void;
  userId: string;
  token: string;
}

export default function Settings({ onClose, userId, token }: SettingsProps) {
  const [providers, setProviders] = useState<ProviderInfo[]>([]);
  const [selectedProvider, setSelectedProvider] = useState<string>('');
  const [selectedModel, setSelectedModel] = useState<string>('');
  const [loading, setLoading] = useState(true);

  const MODEL_KEY = `jcowork_model_${userId}`;

  // Save model selection to localStorage whenever it changes
  const saveModel = (provider: string, model: string) => {
    localStorage.setItem(MODEL_KEY, JSON.stringify({ provider, model }));
  };

  useEffect(() => {
    fetch('/api/providers', {
      headers: { 'Authorization': `Bearer ${token}` },
    })
      .then(r => r.json())
      .then((data: { providers: ProviderInfo[]; default_model: string }) => {
        const providers = data.providers;
        const serverDefault = data.default_model || ''; // e.g. "moonshot:kimi-k2.6"
        setProviders(providers);
        setLoading(false);
        // Auto-select from localStorage or server default
        const saved = localStorage.getItem(MODEL_KEY);
        let initialProvider = '';
        let initialModel = '';
        if (saved) {
          try {
            const parsed = JSON.parse(saved);
            initialProvider = parsed.provider || '';
            initialModel = parsed.model || '';
          } catch {}
        }
        // If no saved preference, use server default
        if (!initialProvider && serverDefault.includes(':')) {
          const [defProvider, defModel] = serverDefault.split(':');
          initialProvider = defProvider;
          initialModel = defModel;
        }
        // Validate saved/default values exist in providers
        const validProvider = providers.find(p => p.id === initialProvider);
        if (validProvider) {
          setSelectedProvider(initialProvider);
          const validModel = validProvider.models.find(m => m.id === initialModel);
          if (validModel) {
            setSelectedModel(initialModel);
          } else if (validProvider.models.length > 0) {
            setSelectedModel(validProvider.models[0].id);
            saveModel(initialProvider, validProvider.models[0].id);
          }
        } else if (providers.length > 0) {
          setSelectedProvider(providers[0].id);
          if (providers[0].models.length > 0) {
            setSelectedModel(providers[0].models[0].id);
            saveModel(providers[0].id, providers[0].models[0].id);
          }
        }
      })
      .catch(() => setLoading(false));
  }, []);

  const currentProvider = providers.find(p => p.id === selectedProvider);
  const currentModel = currentProvider?.models.find(m => m.id === selectedModel);

  const handleProviderChange = (providerId: string) => {
    setSelectedProvider(providerId);
    const provider = providers.find(p => p.id === providerId);
    if (provider && provider.models.length > 0) {
      const model = provider.models[0].id;
      setSelectedModel(model);
      saveModel(providerId, model);
    } else {
      setSelectedModel('');
      saveModel(providerId, '');
    }
  };

  const formatContext = (len: number) => {
    if (len >= 1_000_000) return `${len / 1_000_000}M`;
    if (len >= 1000) return `${len / 1000}K`;
    return `${len}`;
  };

  const selectStyle: React.CSSProperties = {
    width: '100%',
    padding: '10px 12px',
    borderRadius: 8,
    border: '1px solid #444',
    background: '#1a1a1a',
    color: '#eee',
    fontSize: 14,
    outline: 'none',
    cursor: 'pointer',
    appearance: 'none',
    backgroundImage: 'url("data:image/svg+xml,%3Csvg xmlns=\'http://www.w3.org/2000/svg\' width=\'12\' height=\'12\' fill=\'%23888\' viewBox=\'0 0 16 16\'%3E%3Cpath d=\'M8 11L3 6h10z\'/%3E%3C/svg%3E")',
    backgroundRepeat: 'no-repeat',
    backgroundPosition: 'right 12px center',
  };

  const cardStyle: React.CSSProperties = {
    background: '#1a1a1a',
    borderRadius: 12,
    padding: 20,
    border: '1px solid #2a2a2a',
  };

  const labelStyle: React.CSSProperties = {
    display: 'block',
    color: '#888',
    fontSize: 12,
    marginBottom: 8,
    fontWeight: 500,
    textTransform: 'uppercase' as const,
    letterSpacing: '0.5px',
  };

  const tagStyle: React.CSSProperties = {
    display: 'inline-block',
    padding: '2px 8px',
    borderRadius: 4,
    fontSize: 11,
    marginLeft: 8,
    verticalAlign: 'middle',
  };

  return (
    <div style={{ padding: 24, maxWidth: 600, margin: '0 auto' }}>
      <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', marginBottom: 28 }}>
        <h2 style={{ fontSize: 22, fontWeight: 600 }}>Settings</h2>
        <button
          onClick={onClose}
          style={{
            padding: '8px 18px', borderRadius: 8, border: '1px solid #444',
            background: 'transparent', color: '#ccc', cursor: 'pointer', fontSize: 14,
          }}
        >
          Back to Chat
        </button>
      </div>

      {/* LLM Provider Section */}
      <div style={cardStyle}>
        <h3 style={{ fontSize: 16, marginBottom: 16, display: 'flex', alignItems: 'center', gap: 8 }}>
          <span style={{ fontSize: 20 }}>🤖</span> LLM Provider
        </h3>

        {loading ? (
          <div style={{ color: '#888', textAlign: 'center', padding: 20 }}>Loading providers...</div>
        ) : providers.length === 0 ? (
          <div style={{ color: '#f87171', textAlign: 'center', padding: 20 }}>
            No LLM providers registered. Please set API keys in .env
          </div>
        ) : (
          <>
            {/* Provider Select */}
            <div style={{ marginBottom: 20 }}>
              <label style={labelStyle}>Provider</label>
              <select
                value={selectedProvider}
                onChange={e => handleProviderChange(e.target.value)}
                style={selectStyle}
              >
                {providers.map(p => (
                  <option key={p.id} value={p.id}>{p.name}</option>
                ))}
              </select>
              <div style={{ color: '#666', fontSize: 12, marginTop: 4 }}>
                {providers.length} provider{providers.length !== 1 ? 's' : ''} available
              </div>
            </div>

            {/* Model Select */}
            {currentProvider && currentProvider.models.length > 0 && (
              <div style={{ marginBottom: 16 }}>
                <label style={labelStyle}>Model</label>
                <select
                  value={selectedModel}
                  onChange={e => { setSelectedModel(e.target.value); saveModel(selectedProvider, e.target.value); }}
                  style={selectStyle}
                >
                  {currentProvider.models.map(m => (
                    <option key={m.id} value={m.id}>
                      {m.name} ({formatContext(m.context_length)} context)
                    </option>
                  ))}
                </select>
              </div>
            )}

            {/* Current Selection Summary */}
            {currentProvider && currentModel && (
              <div style={{
                marginTop: 16,
                padding: '12px 16px',
                borderRadius: 8,
                background: '#0d1117',
                border: '1px solid #30363d',
              }}>
                <div style={{ fontSize: 13, color: '#888', marginBottom: 4 }}>Active Selection</div>
                <div style={{ fontSize: 15, fontWeight: 500 }}>
                  {currentProvider.name}
                  <span style={{ ...tagStyle, background: '#1f6feb22', color: '#58a6ff' }}>
                    {currentModel.name}
                  </span>
                  <span style={{ ...tagStyle, background: '#23863622', color: '#3fb950' }}>
                    {formatContext(currentModel.context_length)} ctx
                  </span>
                </div>
                <div style={{ fontSize: 12, color: '#666', marginTop: 4 }}>
                  Model ID: <code style={{ color: '#c9d1d9', background: '#161b22', padding: '2px 6px', borderRadius: 4 }}>
                    {selectedProvider}:{selectedModel}
                  </code>
                </div>
              </div>
            )}
          </>
        )}
      </div>

      {/* Memory Section */}
      <div style={{ ...cardStyle, marginTop: 16 }}>
        <h3 style={{ fontSize: 16, marginBottom: 8, display: 'flex', alignItems: 'center', gap: 8 }}>
          <span style={{ fontSize: 20 }}>🧠</span> Memory
        </h3>
        <div style={{ color: '#888', fontSize: 13, lineHeight: 1.6 }}>
          Your agent has persistent memory across sessions. Saved facts include user preferences, environment details, and stable conventions.
        </div>
      </div>

      {/* Skills Section */}
      <div style={{ ...cardStyle, marginTop: 16 }}>
        <h3 style={{ fontSize: 16, marginBottom: 8, display: 'flex', alignItems: 'center', gap: 8 }}>
          <span style={{ fontSize: 20 }}>⚡</span> Skills
        </h3>
        <div style={{ color: '#888', fontSize: 13, lineHeight: 1.6 }}>
          Skills are reusable workflows that the agent creates from experience. They self-improve during use via the patch mechanism.
        </div>
      </div>
    </div>
  );
}
