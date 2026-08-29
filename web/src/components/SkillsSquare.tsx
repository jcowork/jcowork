import { useState, useCallback } from 'react';
import { useT } from '../i18n';
import SkillsList from './SkillsList';
import Connectors from './Connectors';

interface SkillsSquareProps {
  userId: string;
  token: string;
}

export default function SkillsSquare({ userId, token }: SkillsSquareProps) {
  const t = useT();
  const [skillCounts, setSkillCounts] = useState({ enabled: 0, total: 0 });

  const handleCountChange = useCallback((enabled: number, total: number) => {
    setSkillCounts({ enabled, total });
  }, []);

  return (
    <div style={{ display: 'flex', flexDirection: 'column', height: '100%' }}>
      {/* Header */}
      <div style={{
        padding: '16px 24px',
        borderBottom: '1px solid #333',
        flexShrink: 0,
      }}>
        <h2 style={{ fontSize: 20, fontWeight: 600, margin: 0 }}>{t('skillsSquare')}</h2>
        <div style={{ fontSize: 12, color: '#666', marginTop: 2 }}>
          {skillCounts.enabled} {t('skillsEnabled')} · {t('connectorsSubtitle')}
        </div>
      </div>

      {/* Single scrolling page: skills on top, connectors below */}
      <div style={{ flex: 1, overflowY: 'auto', minHeight: 0 }}>
        <SkillsList token={token} onCountChange={handleCountChange} />

        {/* Connectors section */}
        <div style={{
          borderTop: '1px solid #333',
          height: 620,
          display: 'flex',
          flexDirection: 'column',
        }}>
          <Connectors userId={userId} token={token} />
        </div>
      </div>
    </div>
  );
}
