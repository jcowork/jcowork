import { useState, useEffect, useCallback } from 'react';

interface MemoryItem {
  id: string;
  content: string;
  category: string;
}

interface MemoryProps {
  userId: string;
  token: string;
}

export default function Memory({ userId, token }: MemoryProps) {
  const [memories, setMemories] = useState<MemoryItem[]>([]);
  const [loading, setLoading] = useState(true);
  const [searchQuery, setSearchQuery] = useState('');
  const [searchResults, setSearchResults] = useState<MemoryItem[] | null>(null);
  const [editingId, setEditingId] = useState<string | null>(null);
  const [editContent, setEditContent] = useState('');
  const [editCategory, setEditCategory] = useState('');

  const fetchMemories = useCallback(async () => {
    try {
      const res = await fetch('/api/memory', {
        headers: { 'Authorization': `Bearer ${token}` },
      });
      if (res.ok) {
        const data = await res.json();
        setMemories(data);
      }
    } catch (err) {
      console.error('Failed to fetch memories:', err);
    } finally {
      setLoading(false);
    }
  }, [userId]);

  useEffect(() => {
    fetchMemories();
  }, [fetchMemories]);

  const handleSearch = useCallback(async () => {
    if (!searchQuery.trim()) {
      setSearchResults(null);
      return;
    }
    try {
      const res = await fetch(`/api/memory/search?query=${encodeURIComponent(searchQuery)}`, {
        headers: { 'Authorization': `Bearer ${token}` },
      });
      if (res.ok) {
        const data = await res.json();
        setSearchResults(data);
      }
    } catch (err) {
      console.error('Search failed:', err);
    }
  }, [userId, searchQuery]);

  const deleteMemory = async (id: string) => {
    if (!confirm('Delete this memory?')) return;
    try {
      const res = await fetch(`/api/memory/${id}`, {
        method: 'DELETE',
        headers: { 'Authorization': `Bearer ${token}` },
      });
      if (res.ok) {
        setMemories((prev) => prev.filter((m) => m.id !== id));
        if (searchResults) setSearchResults((prev) => prev?.filter((m) => m.id !== id) ?? null);
      }
    } catch (err) {
      console.error('Failed to delete memory:', err);
    }
  };

  const startEdit = (m: MemoryItem) => {
    setEditingId(m.id);
    setEditContent(m.content);
    setEditCategory(m.category);
  };

  const cancelEdit = () => {
    setEditingId(null);
    setEditContent('');
    setEditCategory('');
  };

  const saveEdit = async (id: string) => {
    try {
      const res = await fetch(`/api/memory/${id}`, {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json', 'Authorization': `Bearer ${token}` },
        body: JSON.stringify({ content: editContent, category: editCategory }),
      });
      if (res.ok) {
        const updated = await res.json();
        setMemories((prev) => prev.map((m) => m.id === id ? { ...m, content: updated.content, category: updated.category } : m));
        if (searchResults) setSearchResults((prev) => prev?.map((m) => m.id === id ? { ...m, content: updated.content, category: updated.category } : m) ?? null);
        cancelEdit();
      }
    } catch (err) {
      console.error('Failed to update memory:', err);
    }
  };

  const displayMemories = searchResults ?? memories;

  const categoryColor: Record<string, string> = {
    preference: '#64b5f6',
    fact: '#81c784',
    instruction: '#ffb74d',
    general: '#90a4ae',
  };

  return (
    <div style={{ display: 'flex', flexDirection: 'column', height: '100%' }}>
      {/* Header */}
      <div style={{
        padding: '12px 16px',
        borderBottom: '1px solid #333',
        display: 'flex',
        alignItems: 'center',
        justifyContent: 'space-between',
        gap: 12,
      }}>
        <span style={{ fontWeight: 600, fontSize: 16 }}>Memory</span>
        <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
          <input
            type="text"
            placeholder="Search memories..."
            value={searchQuery}
            onChange={(e) => setSearchQuery(e.target.value)}
            onKeyDown={(e) => e.key === 'Enter' && handleSearch()}
            style={{
              padding: '4px 10px',
              borderRadius: 6,
              border: '1px solid #555',
              background: '#1a1a1a',
              color: '#eee',
              fontSize: 13,
              width: 180,
            }}
          />
          <button
            onClick={handleSearch}
            style={{
              padding: '4px 10px',
              borderRadius: 6,
              border: '1px solid #555',
              background: 'transparent',
              color: '#aaa',
              cursor: 'pointer',
              fontSize: 12,
            }}
          >
            Search
          </button>
          {searchResults && (
            <button
              onClick={() => { setSearchResults(null); setSearchQuery(''); }}
              style={{
                padding: '4px 10px',
                borderRadius: 6,
                border: '1px solid #555',
                background: 'transparent',
                color: '#e57373',
                cursor: 'pointer',
                fontSize: 12,
              }}
            >
              Clear
            </button>
          )}
          <button
            onClick={fetchMemories}
            style={{
              padding: '4px 10px',
              borderRadius: 6,
              border: '1px solid #555',
              background: 'transparent',
              color: '#aaa',
              cursor: 'pointer',
              fontSize: 12,
            }}
          >
            Refresh
          </button>
        </div>
      </div>

      {/* Content */}
      <div style={{ flex: 1, overflowY: 'auto', padding: 16 }}>
        {loading ? (
          <div style={{ color: '#666', padding: 16, textAlign: 'center' }}>Loading...</div>
        ) : displayMemories.length === 0 ? (
          <div style={{
            color: '#666',
            padding: 24,
            textAlign: 'center',
            border: '1px dashed #444',
            borderRadius: 8,
          }}>
            {searchResults !== null ? 'No matching memories found.' : 'No memories yet.'}<br />
            <span style={{ fontSize: 12, color: '#555' }}>
              {searchResults !== null ? 'Try a different search term.' : 'Try saying "记住，我喜欢深色主题" in chat'}
            </span>
          </div>
        ) : (
          <div style={{ display: 'flex', flexDirection: 'column', gap: 8 }}>
            {displayMemories.map((m) => (
              <div
                key={m.id}
                style={{
                  padding: '12px 16px',
                  borderRadius: 8,
                  background: '#2a2a2a',
                  border: '1px solid #444',
                }}
              >
                {editingId === m.id ? (
                  /* Edit mode */
                  <div>
                    <textarea
                      value={editContent}
                      onChange={(e) => setEditContent(e.target.value)}
                      style={{
                        width: '100%',
                        padding: '8px 10px',
                        borderRadius: 6,
                        border: '1px solid #555',
                        background: '#1a1a1a',
                        color: '#eee',
                        fontSize: 14,
                        minHeight: 60,
                        resize: 'vertical',
                        marginBottom: 8,
                        boxSizing: 'border-box',
                      }}
                    />
                    <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
                      <input
                        type="text"
                        value={editCategory}
                        onChange={(e) => setEditCategory(e.target.value)}
                        placeholder="Category"
                        style={{
                          padding: '4px 10px',
                          borderRadius: 6,
                          border: '1px solid #555',
                          background: '#1a1a1a',
                          color: '#eee',
                          fontSize: 13,
                          width: 120,
                        }}
                      />
                      <button
                        onClick={() => saveEdit(m.id)}
                        style={{
                          padding: '4px 14px',
                          borderRadius: 6,
                          border: 'none',
                          background: '#1a73e8',
                          color: '#fff',
                          cursor: 'pointer',
                          fontSize: 13,
                        }}
                      >
                        Save
                      </button>
                      <button
                        onClick={cancelEdit}
                        style={{
                          padding: '4px 14px',
                          borderRadius: 6,
                          border: '1px solid #555',
                          background: 'transparent',
                          color: '#aaa',
                          cursor: 'pointer',
                          fontSize: 13,
                        }}
                      >
                        Cancel
                      </button>
                    </div>
                  </div>
                ) : (
                  /* View mode */
                  <div>
                    <div style={{ display: 'flex', alignItems: 'flex-start', justifyContent: 'space-between' }}>
                      <div style={{ flex: 1 }}>
                        <span
                          style={{
                            display: 'inline-block',
                            padding: '1px 8px',
                            borderRadius: 4,
                            fontSize: 11,
                            fontWeight: 600,
                            color: categoryColor[m.category] || '#90a4ae',
                            background: `${categoryColor[m.category] || '#90a4ae'}20`,
                            marginBottom: 6,
                          }}
                        >
                          {m.category}
                        </span>
                        <div style={{ fontSize: 14, lineHeight: 1.5 }}>{m.content}</div>
                      </div>
                      <div style={{ display: 'flex', gap: 4, marginLeft: 12, flexShrink: 0 }}>
                        <button
                          onClick={() => startEdit(m)}
                          style={{
                            padding: '4px 10px',
                            borderRadius: 4,
                            border: '1px solid #555',
                            background: 'transparent',
                            color: '#64b5f6',
                            cursor: 'pointer',
                            fontSize: 12,
                          }}
                        >
                          Edit
                        </button>
                        <button
                          onClick={() => deleteMemory(m.id)}
                          style={{
                            padding: '4px 10px',
                            borderRadius: 4,
                            border: '1px solid #555',
                            background: 'transparent',
                            color: '#e57373',
                            cursor: 'pointer',
                            fontSize: 12,
                          }}
                        >
                          Delete
                        </button>
                      </div>
                    </div>
                  </div>
                )}
              </div>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}
