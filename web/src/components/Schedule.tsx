import { useState, useEffect, useCallback } from 'react';
import { useT } from '../i18n';

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
}

interface ScheduleProps {
  userId: string;
  token: string;
}

export default function Schedule({ userId, token }: ScheduleProps) {
  const t = useT();
  const [reminders, setReminders] = useState<Reminder[]>([]);
  const [cronJobs, setCronJobs] = useState<CronJob[]>([]);
  const [loading, setLoading] = useState(true);

  const fetchReminders = useCallback(async () => {
    try {
      const res = await fetch('/api/reminders', {
        headers: { 'Authorization': `Bearer ${token}` },
      });
      if (res.ok) {
        const data = await res.json();
        setReminders(data);
      }
    } catch (err) {
      console.error('Failed to fetch reminders:', err);
    }
  }, [userId]);

  const fetchCronJobs = useCallback(async () => {
    try {
      const res = await fetch('/api/cron-jobs', {
        headers: { 'Authorization': `Bearer ${token}` },
      });
      if (res.ok) {
        const data = await res.json();
        setCronJobs(data);
      }
    } catch (err) {
      console.error('Failed to fetch cron jobs:', err);
    }
  }, [userId]);

  useEffect(() => {
    Promise.all([fetchReminders(), fetchCronJobs()]).finally(() => setLoading(false));
    const interval = setInterval(() => { fetchReminders(); fetchCronJobs(); }, 30000);
    return () => clearInterval(interval);
  }, [fetchReminders, fetchCronJobs]);

  const removeReminder = async (id: string) => {
    try {
      const res = await fetch(`/api/reminders/${id}`, { method: 'DELETE', headers: { 'Authorization': `Bearer ${token}` } });
      if (res.ok) {
        setReminders((prev) => prev.filter((r) => r.id !== id));
      }
    } catch (err) {
      console.error('Failed to remove reminder:', err);
    }
  };

  const formatTime = (isoStr: string) => {
    try {
      const d = new Date(isoStr);
      return d.toLocaleString('zh-CN', {
        month: '2-digit',
        day: '2-digit',
        hour: '2-digit',
        minute: '2-digit',
        hour12: false,
      });
    } catch {
      return isoStr;
    }
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

  const removeCronJob = async (id: string) => {
    try {
      const res = await fetch(`/api/cron-jobs/${id}`, { method: 'DELETE', headers: { 'Authorization': `Bearer ${token}` } });
      if (res.ok) {
        setCronJobs((prev) => prev.filter((j) => j.id !== id));
      }
    } catch (err) {
      console.error('Failed to remove cron job:', err);
    }
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
      }}>
        <span style={{ fontWeight: 600, fontSize: 16 }}>{t('schedule')}</span>
        <button
          onClick={() => { fetchReminders(); fetchCronJobs(); }}
          style={{
            padding: '4px 12px',
            borderRadius: 6,
            border: '1px solid #555',
            background: 'transparent',
            color: '#aaa',
            cursor: 'pointer',
            fontSize: 12,
          }}
        >
          {t('refresh')}
        </button>
      </div>

      {/* Content */}
      <div style={{ flex: 1, overflowY: 'auto', padding: 16 }}>
        {/* Reminders section */}
        <div style={{ marginBottom: 24 }}>
          <h3 style={{ fontSize: 14, color: '#888', textTransform: 'uppercase', marginBottom: 12, letterSpacing: 1 }}>
            🔔 {t('reminders')}
          </h3>

          {loading ? (
            <div style={{ color: '#666', padding: 16, textAlign: 'center' }}>{t('loading')}</div>
          ) : reminders.length === 0 ? (
            <div style={{
              color: '#666',
              padding: 24,
              textAlign: 'center',
              border: '1px dashed #444',
              borderRadius: 8,
            }}>
              {t('noReminders')}<br />
              <span style={{ fontSize: 12, color: '#555' }}>
                {t('trySayingReminder')}
              </span>
            </div>
          ) : (
            <div style={{ display: 'flex', flexDirection: 'column', gap: 8 }}>
              {reminders.map((r) => (
                <div
                  key={r.id}
                  style={{
                    padding: '12px 16px',
                    borderRadius: 8,
                    background: '#2a2a2a',
                    border: '1px solid #444',
                    display: 'flex',
                    alignItems: 'center',
                    justifyContent: 'space-between',
                  }}
                >
                  <div style={{ flex: 1 }}>
                    <div style={{ fontWeight: 500, marginBottom: 4 }}>{r.message}</div>
                    <div style={{ fontSize: 12, color: '#888' }}>
                      ⏰ {formatTime(r.fire_at)} · <span style={{ color: '#6a9fd8' }}>{timeUntil(r.fire_at)}</span>
                    </div>
                  </div>
                  <button
                    onClick={() => removeReminder(r.id)}
                    style={{
                      padding: '4px 10px',
                      borderRadius: 4,
                      border: '1px solid #555',
                      background: 'transparent',
                      color: '#e57373',
                      cursor: 'pointer',
                      fontSize: 12,
                      marginLeft: 12,
                    }}
                  >
                    {t('cancel')}
                  </button>
                </div>
              ))}
            </div>
          )}
        </div>

        {/* Cron Jobs section */}
        <div>
          <h3 style={{ fontSize: 14, color: '#888', textTransform: 'uppercase', marginBottom: 12, letterSpacing: 1 }}>
            {t('cronJobs')}
          </h3>

          {loading ? (
            <div style={{ color: '#666', padding: 16, textAlign: 'center' }}>{t('loading')}</div>
          ) : cronJobs.length === 0 ? (
            <div style={{
              color: '#666',
              padding: 24,
              textAlign: 'center',
              border: '1px dashed #444',
              borderRadius: 8,
            }}>
              {t('noCronJobs')}<br />
              <span style={{ fontSize: 12, color: '#555' }}>
                {t('trySayingCron')}
              </span>
            </div>
          ) : (
            <div style={{ display: 'flex', flexDirection: 'column', gap: 8 }}>
              {cronJobs.map((j) => (
                <div
                  key={j.id}
                  style={{
                    padding: '12px 16px',
                    borderRadius: 8,
                    background: '#2a2a2a',
                    border: '1px solid #444',
                    display: 'flex',
                    alignItems: 'center',
                    justifyContent: 'space-between',
                  }}
                >
                  <div style={{ flex: 1 }}>
                    <div style={{ fontWeight: 500, marginBottom: 4 }}>{j.prompt}</div>
                    <div style={{ fontSize: 12, color: '#888' }}>
                      <span style={{
                        display: 'inline-block',
                        padding: '1px 6px',
                        borderRadius: 4,
                        fontSize: 11,
                        fontWeight: 600,
                        color: '#81c784',
                        background: '#81c78420',
                        marginRight: 8,
                      }}>{j.schedule}</span>
                      {j.last_run && <span>{t('lastRunLabel')}: {formatTime(j.last_run)}</span>}
                    </div>
                  </div>
                  <button
                    onClick={() => removeCronJob(j.id)}
                    style={{
                      padding: '4px 10px',
                      borderRadius: 4,
                      border: '1px solid #555',
                      background: 'transparent',
                      color: '#e57373',
                      cursor: 'pointer',
                      fontSize: 12,
                      marginLeft: 12,
                    }}
                  >
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
