import { useState, useEffect, useCallback } from 'react';
import { useT } from '../i18n';
import { formatFrequency, type TranslationFn } from '../utils/cron';

interface Reminder {
  id: string;
  user_id: string;
  fire_at: string;
  message: string;
  triggered: boolean;
}

interface CronJob {
  id: string;
  user_id: string;
  schedule: string;
  prompt: string;
  enabled: boolean;
  last_run: string | null;
  created_at: string;
  name?: string;
  model?: string;
}

interface TaskResult {
  id: string;
  cron_job_id: string;
  user_id: string;
  output: string;
  status: string;
  executed_at: string;
}

interface ModelInfo {
  id: string;
  name: string;
  context_length: number;
}

interface ProviderEntry {
  id: string;
  name: string;
  models: ModelInfo[];
}

interface ScheduleProps {
  userId: string;
  token: string;
}

type Frequency = 'hourly' | 'daily' | 'weekly' | 'monthly' | 'yearly';

export default function Schedule({ userId: _userId, token }: ScheduleProps) {
  const t = useT();
  const [reminders, setReminders] = useState<Reminder[]>([]);
  const [cronJobs, setCronJobs] = useState<CronJob[]>([]);
  const [loading, setLoading] = useState(true);
  const [showForm, setShowForm] = useState(false);
  const [providers, setProviders] = useState<ProviderEntry[]>([]);

  // Form state
  const [taskName, setTaskName] = useState('');
  const [taskPrompt, setTaskPrompt] = useState('');
  const [taskModel, setTaskModel] = useState('');
  const [frequency, setFrequency] = useState<Frequency>('daily');
  const [second, setSecond] = useState(0);
  const [minute, setMinute] = useState(0);
  const [hour, setHour] = useState(9);
  const [day, setDay] = useState(1);
  const [month, setMonth] = useState(1);
  const [daysOfWeek, setDaysOfWeek] = useState<number[]>([1]); // For multi-select weekly, 0=Sun, 1=Mon, ..., 6=Sat
  const [submitting, setSubmitting] = useState(false);
  const [submitStatus, setSubmitStatus] = useState<'idle' | 'success' | 'error'>('idle');

  // Collapsed results per job
  const [expandedResults, setExpandedResults] = useState<Record<string, boolean>>({});
  const [taskResults, setTaskResults] = useState<Record<string, TaskResult[]>>({});

  const fetchReminders = useCallback(async () => {
    try {
      const res = await fetch('/api/reminders', {
        headers: { 'Authorization': `Bearer ${token}` },
      });
      if (res.ok) setReminders(await res.json());
    } catch (err) {
      console.error('Failed to fetch reminders:', err);
    }
  }, [token]);

  const fetchCronJobs = useCallback(async () => {
    try {
      const res = await fetch('/api/cron-jobs', {
        headers: { 'Authorization': `Bearer ${token}` },
      });
      if (res.ok) setCronJobs(await res.json());
    } catch (err) {
      console.error('Failed to fetch cron jobs:', err);
    }
  }, [token]);

  const fetchProviders = useCallback(async () => {
    try {
      const res = await fetch('/api/providers/entries', {
        headers: { 'Authorization': `Bearer ${token}` },
      });
      if (res.ok) {
        const data = await res.json();
        setProviders(data.entries || []);
        // Auto-select first model if none selected
        if (!taskModel && data.entries?.length > 0) {
          const firstProvider = data.entries[0];
          if (firstProvider.models?.length > 0) {
            setTaskModel(`${firstProvider.id}:${firstProvider.models[0].id}`);
          }
        }
      }
    } catch (err) {
      console.error('Failed to fetch providers:', err);
    }
  }, [token]);

  useEffect(() => {
    Promise.all([fetchReminders(), fetchCronJobs(), fetchProviders()]).finally(() => setLoading(false));
    const interval = setInterval(() => { fetchReminders(); fetchCronJobs(); }, 30000);
    return () => clearInterval(interval);
  }, [fetchReminders, fetchCronJobs, fetchProviders]);

  const fetchTaskResults = async (jobId: string) => {
    try {
      const res = await fetch(`/api/cron-jobs/${jobId}/results`, {
        headers: { 'Authorization': `Bearer ${token}` },
      });
      if (res.ok) {
        const results = await res.json();
        setTaskResults(prev => ({ ...prev, [jobId]: results }));
      }
    } catch (err) {
      console.error('Failed to fetch task results:', err);
    }
  };

  const toggleResults = (jobId: string) => {
    const isExpanded = expandedResults[jobId];
    if (!isExpanded) {
      fetchTaskResults(jobId);
    }
    setExpandedResults(prev => ({ ...prev, [jobId]: !prev[jobId] }));
  };

  const removeReminder = async (id: string) => {
    try {
      const res = await fetch(`/api/reminders/${id}`, { method: 'DELETE', headers: { 'Authorization': `Bearer ${token}` } });
      if (res.ok) setReminders(prev => prev.filter(r => r.id !== id));
    } catch (err) {
      console.error('Failed to remove reminder:', err);
    }
  };

  const removeCronJob = async (id: string) => {
    try {
      const res = await fetch(`/api/cron-jobs/${id}`, { method: 'DELETE', headers: { 'Authorization': `Bearer ${token}` } });
      if (res.ok) setCronJobs(prev => prev.filter(j => j.id !== id));
    } catch (err) {
      console.error('Failed to remove cron job:', err);
    }
  };

  const handleSubmitTask = async () => {
    if (!taskName.trim() || !taskPrompt.trim() || !taskModel) return;
    setSubmitting(true);
    setSubmitStatus('idle');
    try {
      const res = await fetch('/api/cron-jobs', {
        method: 'POST',
        headers: {
          'Authorization': `Bearer ${token}`,
          'Content-Type': 'application/json',
        },
        body: JSON.stringify({
          name: taskName.trim(),
          prompt: taskPrompt.trim(),
          model: taskModel,
          frequency,
          second,
          minute,
          hour,
          // Only send day for monthly/yearly frequency
          day: (frequency === 'monthly' || frequency === 'yearly') ? day : undefined,
          // Only send month for yearly frequency
          month: frequency === 'yearly' ? month : undefined,
          // Only send days_of_week for weekly frequency
          days_of_week: frequency === 'weekly' ? daysOfWeek : undefined,
        }),
      });
      if (res.ok) {
        setSubmitStatus('success');
        setTaskName('');
        setTaskPrompt('');
        setShowForm(false);
        fetchCronJobs();
        setTimeout(() => setSubmitStatus('idle'), 3000);
      } else {
        setSubmitStatus('error');
      }
    } catch {
      setSubmitStatus('error');
    } finally {
      setSubmitting(false);
    }
  };

  const formatTime = (isoStr: string) => {
    try {
      return new Date(isoStr).toLocaleString('zh-CN', {
        month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit', hour12: false,
      });
    } catch { return isoStr; }
  };

  const timeUntil = (isoStr: string) => {
    const diff = new Date(isoStr).getTime() - Date.now();
    if (diff <= 0) return '已到期';
    const mins = Math.floor(diff / 60000);
    if (mins < 60) return `${mins}分钟后`;
    const hours = Math.floor(mins / 60);
    const remainMins = mins % 60;
    if (hours < 24) return `${hours}小时${remainMins > 0 ? remainMins + '分钟' : ''}后`;
    const days = Math.floor(hours / 24);
    return `${days}天后`;
  };

  const formatJobFrequency = (job: CronJob) => {
    return formatFrequency(job.schedule, t as TranslationFn);
  };

  const formatModelName = (modelStr?: string) => {
    if (!modelStr) return '';
    const parts = modelStr.split(':');
    return parts.length > 1 ? parts[1] : modelStr;
  };

  // ── Styles ──
  const inputStyle: React.CSSProperties = {
    width: '100%', padding: '8px 10px', borderRadius: 6,
    border: '1px solid #444', background: '#1a1a1a', color: '#eee',
    fontSize: 13, outline: 'none', boxSizing: 'border-box',
  };
  const labelStyle: React.CSSProperties = {
    display: 'block', color: '#888', fontSize: 11, marginBottom: 4,
    fontWeight: 500, textTransform: 'uppercase' as const, letterSpacing: '0.5px',
  };
  const btnPrimary: React.CSSProperties = {
    padding: '8px 16px', borderRadius: 6, border: 'none',
    background: '#1f6feb', color: '#fff', cursor: 'pointer',
    fontSize: 13, fontWeight: 500,
  };
  const btnSecondary: React.CSSProperties = {
    padding: '6px 14px', borderRadius: 6, border: '1px solid #444',
    background: 'transparent', color: '#ccc', cursor: 'pointer', fontSize: 13,
  };

  return (
    <div style={{ display: 'flex', flexDirection: 'column', height: '100%' }}>
      {/* Header */}
      <div style={{
        padding: '12px 16px', borderBottom: '1px solid #333',
        display: 'flex', alignItems: 'center', justifyContent: 'space-between',
      }}>
        <span style={{ fontWeight: 600, fontSize: 16 }}>{t('schedule')}</span>
        <div style={{ display: 'flex', gap: 8 }}>
          <button onClick={() => setShowForm(!showForm)} style={{
            ...btnPrimary,
            background: showForm ? '#444' : '#1f6feb',
          }}>
            {showForm ? '✕' : `+ ${t('addNewTask')}`}
          </button>
          <button onClick={() => { fetchReminders(); fetchCronJobs(); }} style={{
            padding: '4px 12px', borderRadius: 6, border: '1px solid #555',
            background: 'transparent', color: '#aaa', cursor: 'pointer', fontSize: 12,
          }}>
            {t('refresh')}
          </button>
        </div>
      </div>

      {/* Content */}
      <div style={{ flex: 1, overflowY: 'auto', padding: 16 }}>
        {/* ── Add New Task Form ── */}
        {showForm && (
          <div style={{
            padding: 16, borderRadius: 8, border: '1px solid #1f6feb44',
            background: '#0d1117', marginBottom: 20,
          }}>
            <h4 style={{ fontSize: 14, marginBottom: 14, color: '#58a6ff', margin: '0 0 14px 0' }}>
              {t('addNewTask')}
            </h4>

            {/* Task Name */}
            <div style={{ marginBottom: 12 }}>
              <label style={labelStyle}>{t('taskName')}</label>
              <input
                value={taskName}
                onChange={e => setTaskName(e.target.value)}
                placeholder={t('taskNamePlaceholder')}
                style={inputStyle}
              />
            </div>

            {/* Prompt Description */}
            <div style={{ marginBottom: 12 }}>
              <label style={labelStyle}>{t('promptDesc')}</label>
              <textarea
                value={taskPrompt}
                onChange={e => setTaskPrompt(e.target.value)}
                placeholder={t('promptDescPlaceholder')}
                rows={3}
                style={{ ...inputStyle, resize: 'vertical', fontFamily: 'inherit' }}
              />
            </div>

            {/* Execution Model */}
            <div style={{ marginBottom: 12 }}>
              <label style={labelStyle}>{t('executionModel')}</label>
              <select
                value={taskModel}
                onChange={e => setTaskModel(e.target.value)}
                style={{ ...inputStyle, cursor: 'pointer' }}
              >
                <option value="">{t('selectModel')}</option>
                {providers.map(p => (
                  <optgroup key={p.id} label={p.name}>
                    {p.models.map(m => (
                      <option key={`${p.id}:${m.id}`} value={`${p.id}:${m.id}`}>
                        {m.name || m.id}
                      </option>
                    ))}
                  </optgroup>
                ))}
              </select>
            </div>

            {/* Execution Frequency */}
            <div style={{ marginBottom: 12 }}>
              <label style={labelStyle}>{t('executionFrequency')}</label>
              <div style={{ display: 'flex', gap: 6, marginBottom: 8, flexWrap: 'wrap' }}>
                {(['hourly', 'daily', 'weekly', 'monthly', 'yearly'] as Frequency[]).map(f => (
                  <button key={f} onClick={() => {
                    setFrequency(f);
                    // Reset values when switching frequencies
                    if (f !== 'monthly' && f !== 'yearly') setDay(1);
                    if (f !== 'yearly') setMonth(1);
                    if (f !== 'weekly') setDaysOfWeek([1]);
                  }} style={{
                    flex: '1 1 auto', padding: '8px 12px', borderRadius: 6, cursor: 'pointer',
                    fontSize: 12, fontWeight: 500, textAlign: 'center', whiteSpace: 'nowrap',
                    border: frequency === f ? '1px solid #1f6feb' : '1px solid #444',
                    background: frequency === f ? '#1f6feb22' : 'transparent',
                    color: frequency === f ? '#58a6ff' : '#aaa',
                  }}>
                    {t(f === 'hourly' ? 'frequencyHourly' : f === 'daily' ? 'frequencyDaily' : f === 'weekly' ? 'frequencyWeekly' : f === 'monthly' ? 'frequencyMonthly' : 'frequencyYearly')}
                  </button>
                ))}
              </div>

              {/* Time inputs based on frequency */}
              <div style={{ display: 'flex', gap: 10 }}>
                {(frequency === 'hourly') && (
                  <div style={{ flex: 1 }}>
                    <label style={labelStyle}>{t('specificMinute')}</label>
                    <div style={{ display: 'flex', gap: 6, alignItems: 'center' }}>
                      <input type="number" min={0} max={59} value={minute}
                        onChange={e => setMinute(parseInt(e.target.value) || 0)}
                        style={{ ...inputStyle, textAlign: 'center', width: 60 }} />
                      <span style={{ color: '#666', fontSize: 13 }}>{t('minuteUnit')}</span>
                      <span style={{ color: '#666' }}>:</span>
                      <input type="number" min={0} max={59} value={second}
                        onChange={e => setSecond(parseInt(e.target.value) || 0)}
                        style={{ ...inputStyle, textAlign: 'center', width: 60 }} />
                      <span style={{ color: '#666', fontSize: 13 }}>{t('secondUnit')}</span>
                    </div>
                  </div>
                )}
                {(frequency === 'daily') && (
                  <>
                    <div style={{ flex: 1 }}>
                      <label style={labelStyle}>{t('specificTime')}</label>
                      <div style={{ display: 'flex', gap: 6, alignItems: 'center' }}>
                        <input type="number" min={0} max={23} value={hour}
                          onChange={e => setHour(parseInt(e.target.value) || 0)}
                          style={{ ...inputStyle, textAlign: 'center', width: 60 }} />
                        <span style={{ color: '#666', fontSize: 13 }}>{t('hourUnit')}</span>
                        <span style={{ color: '#666' }}>:</span>
                        <input type="number" min={0} max={59} value={minute}
                          onChange={e => setMinute(parseInt(e.target.value) || 0)}
                          style={{ ...inputStyle, textAlign: 'center', width: 60 }} />
                        <span style={{ color: '#666', fontSize: 13 }}>{t('minuteUnit')}</span>
                        <span style={{ color: '#666' }}>:</span>
                        <input type="number" min={0} max={59} value={second}
                          onChange={e => setSecond(parseInt(e.target.value) || 0)}
                          style={{ ...inputStyle, textAlign: 'center', width: 60 }} />
                        <span style={{ color: '#666', fontSize: 13 }}>{t('secondUnit')}</span>
                      </div>
                    </div>
                  </>
                )}
                {frequency === 'weekly' && (
                  <div style={{ flex: 1 }}>
                    <label style={labelStyle}>{t('dayOfWeek')} & {t('specificTime')}</label>
                    <div style={{ display: 'flex', gap: 4, marginBottom: 6 }}>
                      {['日', '一', '二', '三', '四', '五', '六'].map((dayName, idx) => {
                        const isSelected = daysOfWeek.includes(idx);
                        return (
                          <button key={idx} onClick={() => {
                            if (isSelected) {
                              // Remove day, but ensure at least one day remains
                              if (daysOfWeek.length > 1) {
                                setDaysOfWeek(daysOfWeek.filter(d => d !== idx));
                              }
                            } else {
                              // Add day
                              setDaysOfWeek([...daysOfWeek, idx].sort((a, b) => a - b));
                            }
                          }} style={{
                            flex: 1, padding: '6px 0', borderRadius: 4, cursor: 'pointer',
                            fontSize: 12, fontWeight: 500, textAlign: 'center',
                            border: isSelected ? '1px solid #1f6feb' : '1px solid #444',
                            background: isSelected ? '#1f6feb' : 'transparent',
                            color: isSelected ? '#fff' : '#aaa',
                          }}>
                            {dayName}
                          </button>
                        );
                      })}
                    </div>
                    <div style={{ display: 'flex', gap: 6, alignItems: 'center' }}>
                      <input type="number" min={0} max={23} value={hour}
                        onChange={e => setHour(parseInt(e.target.value) || 0)}
                        style={{ ...inputStyle, textAlign: 'center', width: 60 }} />
                      <span style={{ color: '#666', fontSize: 13 }}>{t('hourUnit')}</span>
                      <span style={{ color: '#666' }}>:</span>
                      <input type="number" min={0} max={59} value={minute}
                        onChange={e => setMinute(parseInt(e.target.value) || 0)}
                        style={{ ...inputStyle, textAlign: 'center', width: 60 }} />
                      <span style={{ color: '#666', fontSize: 13 }}>{t('minuteUnit')}</span>
                      <span style={{ color: '#666' }}>:</span>
                      <input type="number" min={0} max={59} value={second}
                        onChange={e => setSecond(parseInt(e.target.value) || 0)}
                        style={{ ...inputStyle, textAlign: 'center', width: 60 }} />
                      <span style={{ color: '#666', fontSize: 13 }}>{t('secondUnit')}</span>
                    </div>
                  </div>
                )}
                {frequency === 'monthly' && (
                  <div style={{ flex: 1 }}>
                    <label style={labelStyle}>{t('specificDay')} & {t('specificTime')}</label>
                    <div style={{ display: 'flex', gap: 6, alignItems: 'center' }}>
                      <input type="number" min={1} max={28} value={day}
                        onChange={e => setDay(parseInt(e.target.value) || 1)}
                        style={{ ...inputStyle, textAlign: 'center', width: 60 }} />
                      <span style={{ color: '#666', fontSize: 13 }}>{t('dayOfMonth')}</span>
                      <input type="number" min={0} max={23} value={hour}
                        onChange={e => setHour(parseInt(e.target.value) || 0)}
                        style={{ ...inputStyle, textAlign: 'center', width: 60 }} />
                      <span style={{ color: '#666', fontSize: 13 }}>{t('hourUnit')}</span>
                      <span style={{ color: '#666' }}>:</span>
                      <input type="number" min={0} max={59} value={minute}
                        onChange={e => setMinute(parseInt(e.target.value) || 0)}
                        style={{ ...inputStyle, textAlign: 'center', width: 60 }} />
                      <span style={{ color: '#666', fontSize: 13 }}>{t('minuteUnit')}</span>
                      <span style={{ color: '#666' }}>:</span>
                      <input type="number" min={0} max={59} value={second}
                        onChange={e => setSecond(parseInt(e.target.value) || 0)}
                        style={{ ...inputStyle, textAlign: 'center', width: 60 }} />
                      <span style={{ color: '#666', fontSize: 13 }}>{t('secondUnit')}</span>
                    </div>
                  </div>
                )}
                {frequency === 'yearly' && (
                  <div style={{ flex: 1 }}>
                    <label style={labelStyle}>{t('specificMonth')} & {t('specificDay')} & {t('specificTime')}</label>
                    <div style={{ display: 'flex', gap: 6, alignItems: 'center', flexWrap: 'wrap' }}>
                      <select value={month} onChange={e => setMonth(parseInt(e.target.value) || 1)}
                        style={{ ...inputStyle, width: 70, cursor: 'pointer' }}>
                        {['monthJanuary', 'monthFebruary', 'monthMarch', 'monthApril', 'monthMay', 'monthJune',
                          'monthJuly', 'monthAugust', 'monthSeptember', 'monthOctober', 'monthNovember', 'monthDecember'
                        ].map((key, idx) => (
                          <option key={idx} value={idx + 1}>{t(key as any)}</option>
                        ))}
                      </select>
                      <input type="number" min={1} max={28} value={day}
                        onChange={e => setDay(parseInt(e.target.value) || 1)}
                        style={{ ...inputStyle, textAlign: 'center', width: 60 }} />
                      <span style={{ color: '#666', fontSize: 13 }}>{t('dayOfMonth')}</span>
                      <input type="number" min={0} max={23} value={hour}
                        onChange={e => setHour(parseInt(e.target.value) || 0)}
                        style={{ ...inputStyle, textAlign: 'center', width: 60 }} />
                      <span style={{ color: '#666', fontSize: 13 }}>{t('hourUnit')}</span>
                      <span style={{ color: '#666' }}>:</span>
                      <input type="number" min={0} max={59} value={minute}
                        onChange={e => setMinute(parseInt(e.target.value) || 0)}
                        style={{ ...inputStyle, textAlign: 'center', width: 60 }} />
                      <span style={{ color: '#666', fontSize: 13 }}>{t('minuteUnit')}</span>
                      <span style={{ color: '#666' }}>:</span>
                      <input type="number" min={0} max={59} value={second}
                        onChange={e => setSecond(parseInt(e.target.value) || 0)}
                        style={{ ...inputStyle, textAlign: 'center', width: 60 }} />
                      <span style={{ color: '#666', fontSize: 13 }}>{t('secondUnit')}</span>
                    </div>
                  </div>
                )}
              </div>
            </div>

            {/* Submit */}
            <div style={{ display: 'flex', gap: 8, justifyContent: 'flex-end', alignItems: 'center' }}>
              {submitStatus === 'success' && (
                <span style={{ color: '#3fb950', fontSize: 13 }}>{t('taskCreated')}</span>
              )}
              {submitStatus === 'error' && (
                <span style={{ color: '#f87171', fontSize: 13 }}>{t('createFailed')}</span>
              )}
              <button onClick={() => { setShowForm(false); setSubmitStatus('idle'); }} style={btnSecondary}>
                {t('cancel')}
              </button>
              <button
                onClick={handleSubmitTask}
                disabled={submitting || !taskName.trim() || !taskPrompt.trim() || !taskModel}
                style={{
                  ...btnPrimary,
                  opacity: (submitting || !taskName.trim() || !taskPrompt.trim() || !taskModel) ? 0.5 : 1,
                  cursor: (submitting || !taskName.trim() || !taskPrompt.trim() || !taskModel) ? 'not-allowed' : 'pointer',
                }}
              >
                {submitting ? t('creating') : t('createTask')}
              </button>
            </div>
          </div>
        )}

        {/* ── Periodic Tasks List ── */}
        <div style={{ marginBottom: 24 }}>
          <h3 style={{ fontSize: 14, color: '#888', textTransform: 'uppercase', marginBottom: 12, letterSpacing: 1 }}>
            📋 {t('schedule')}
          </h3>

          {loading ? (
            <div style={{ color: '#666', padding: 16, textAlign: 'center' }}>{t('loading')}</div>
          ) : cronJobs.length === 0 ? (
            <div style={{
              color: '#666', padding: 24, textAlign: 'center',
              border: '1px dashed #444', borderRadius: 8,
            }}>
              {t('noCronJobs')}<br />
              <span style={{ fontSize: 12, color: '#555' }}>
                {t('trySayingCron')}
              </span>
            </div>
          ) : (
            <div style={{ display: 'flex', flexDirection: 'column', gap: 10 }}>
              {cronJobs.map(job => (
                <div key={job.id} style={{
                  borderRadius: 8, background: '#2a2a2a', border: '1px solid #444',
                  overflow: 'hidden',
                }}>
                  {/* Task header */}
                  <div style={{
                    padding: '12px 16px', display: 'flex', alignItems: 'center',
                    justifyContent: 'space-between',
                  }}>
                    <div style={{ flex: 1 }}>
                      <div style={{ fontWeight: 500, marginBottom: 4, fontSize: 14 }}>
                        {job.name || job.prompt}
                      </div>
                      <div style={{ fontSize: 12, color: '#888', display: 'flex', gap: 8, flexWrap: 'wrap', alignItems: 'center' }}>
                        <span style={{
                          display: 'inline-block', padding: '1px 6px', borderRadius: 4,
                          fontSize: 11, fontWeight: 600, color: '#81c784', background: '#81c78420',
                        }}>
                          {formatJobFrequency(job)}
                        </span>
                        {job.model && (
                          <span style={{
                            display: 'inline-block', padding: '1px 6px', borderRadius: 4,
                            fontSize: 11, fontWeight: 600, color: '#6a9fd8', background: '#6a9fd820',
                          }}>
                            {formatModelName(job.model)}
                          </span>
                        )}
                        {job.last_run && (
                          <span>{t('lastRunLabel')}: {formatTime(job.last_run)}</span>
                        )}
                      </div>
                      {job.name && job.prompt && (
                        <div style={{ fontSize: 12, color: '#666', marginTop: 4, lineHeight: 1.4 }}>
                          {job.prompt.length > 100 ? job.prompt.slice(0, 100) + '...' : job.prompt}
                        </div>
                      )}
                    </div>
                    <div style={{ display: 'flex', gap: 6, marginLeft: 12 }}>
                      <button onClick={() => toggleResults(job.id)} style={{
                        padding: '4px 10px', borderRadius: 4, border: '1px solid #555',
                        background: 'transparent', color: '#aaa', cursor: 'pointer', fontSize: 11,
                      }}>
                        {expandedResults[job.id] ? '▼' : '▶'} {t('executionResults')}
                      </button>
                      <button onClick={() => removeCronJob(job.id)} style={{
                        padding: '4px 10px', borderRadius: 4, border: '1px solid #555',
                        background: 'transparent', color: '#e57373', cursor: 'pointer', fontSize: 12,
                      }}>
                        {t('cancel')}
                      </button>
                    </div>
                  </div>

                  {/* Collapsible execution results */}
                  {expandedResults[job.id] && (
                    <div style={{
                      borderTop: '1px solid #333', padding: '10px 16px',
                      background: '#1e1e1e', maxHeight: 300, overflowY: 'auto',
                    }}>
                      {!taskResults[job.id] || taskResults[job.id].length === 0 ? (
                        <div style={{ color: '#555', fontSize: 12, textAlign: 'center', padding: 12 }}>
                          {t('noResults')}
                        </div>
                      ) : (
                        <div style={{ display: 'flex', flexDirection: 'column', gap: 8 }}>
                          {taskResults[job.id].map(result => (
                            <div key={result.id} style={{
                              padding: '8px 12px', borderRadius: 6,
                              background: '#2a2a2a', border: '1px solid #383838',
                            }}>
                              <div style={{
                                display: 'flex', justifyContent: 'space-between',
                                alignItems: 'center', marginBottom: 6,
                              }}>
                                <span style={{
                                  fontSize: 11, fontWeight: 600, padding: '1px 6px', borderRadius: 4,
                                  color: result.status === 'success' ? '#81c784' : '#e57373',
                                  background: result.status === 'success' ? '#81c78420' : '#e5737320',
                                }}>
                                  {result.status === 'success' ? t('success') : t('failed')}
                                </span>
                                <span style={{ fontSize: 11, color: '#666' }}>
                                  {formatTime(result.executed_at)}
                                </span>
                              </div>
                              <div style={{
                                fontSize: 12, color: '#bbb', lineHeight: 1.5,
                                whiteSpace: 'pre-wrap', wordBreak: 'break-word',
                              }}>
                                {result.output.length > 500
                                  ? result.output.slice(0, 500) + '...'
                                  : result.output}
                              </div>
                            </div>
                          ))}
                        </div>
                      )}
                    </div>
                  )}
                </div>
              ))}
            </div>
          )}
        </div>

        {/* ── Reminders section ── */}
        <div>
          <h3 style={{ fontSize: 14, color: '#888', textTransform: 'uppercase', marginBottom: 12, letterSpacing: 1 }}>
            🔔 {t('reminders')}
          </h3>

          {loading ? (
            <div style={{ color: '#666', padding: 16, textAlign: 'center' }}>{t('loading')}</div>
          ) : reminders.length === 0 ? (
            <div style={{
              color: '#666', padding: 24, textAlign: 'center',
              border: '1px dashed #444', borderRadius: 8,
            }}>
              {t('noReminders')}<br />
              <span style={{ fontSize: 12, color: '#555' }}>
                {t('trySayingReminder')}
              </span>
            </div>
          ) : (
            <div style={{ display: 'flex', flexDirection: 'column', gap: 8 }}>
              {reminders.map(r => (
                <div key={r.id} style={{
                  padding: '12px 16px', borderRadius: 8, background: '#2a2a2a',
                  border: '1px solid #444', display: 'flex', alignItems: 'center',
                  justifyContent: 'space-between',
                }}>
                  <div style={{ flex: 1 }}>
                    <div style={{ fontWeight: 500, marginBottom: 4 }}>{r.message}</div>
                    <div style={{ fontSize: 12, color: '#888' }}>
                      ⏰ {formatTime(r.fire_at)} · <span style={{ color: '#6a9fd8' }}>{timeUntil(r.fire_at)}</span>
                    </div>
                  </div>
                  <button onClick={() => removeReminder(r.id)} style={{
                    padding: '4px 10px', borderRadius: 4, border: '1px solid #555',
                    background: 'transparent', color: '#e57373', cursor: 'pointer',
                    fontSize: 12, marginLeft: 12,
                  }}>
                    {t('cancel')}
                  </button>
                </div>
              ))}
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
