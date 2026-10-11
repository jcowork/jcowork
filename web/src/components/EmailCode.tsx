import { useT } from '../i18n';

const inputStyle: React.CSSProperties = {
  width: '100%', padding: 10, marginBottom: 12, borderRadius: 8,
  border: '1px solid #555', background: '#1a1a1a', color: '#eee', fontSize: 16,
  boxSizing: 'border-box',
};

interface EmailCodeFieldsProps {
  email: string;
  code: string;
  onEmailChange: (value: string) => void;
  onCodeChange: (value: string) => void;
  onSendCode: () => void;
  /** Seconds left on the resend cooldown; 0 means the button is live. */
  countdown: number;
  sending: boolean;
  autoFocus?: boolean;
}

/**
 * Email address + "get code" button + code input.
 *
 * Sibling of `PhoneCodeFields` for the email channel: registration and
 * password reset both collect these two values against
 * `/api/auth/email/send`.
 */
export default function EmailCodeFields({
  email, code, onEmailChange, onCodeChange, onSendCode, countdown, sending, autoFocus = true,
}: EmailCodeFieldsProps) {
  const t = useT();
  const busy = sending || countdown > 0;
  return (
    <>
      <div style={{ display: 'flex', gap: 8, marginBottom: 12 }}>
        <input
          type="email"
          autoComplete="email"
          placeholder={t('emailAddress')}
          value={email}
          onChange={(e) => onEmailChange(e.target.value)}
          maxLength={254}
          autoFocus={autoFocus}
          style={{ ...inputStyle, flex: 1, width: 'auto', marginBottom: 0 }}
        />
        <button
          type="button"
          onClick={onSendCode}
          disabled={busy}
          style={{
            padding: '0 14px', borderRadius: 8, flexShrink: 0, whiteSpace: 'nowrap',
            border: `1px solid ${busy ? '#444' : '#1f6feb'}`,
            background: busy ? '#222' : '#1f6feb22',
            color: busy ? '#666' : '#58a6ff',
            fontSize: 14, cursor: busy ? 'not-allowed' : 'pointer',
          }}
        >
          {countdown > 0 ? t('resendIn').replace('{n}', String(countdown)) : t('getCode')}
        </button>
      </div>
      <input
        type="text"
        inputMode="numeric"
        autoComplete="one-time-code"
        placeholder={t('emailCode')}
        value={code}
        onChange={(e) => onCodeChange(e.target.value)}
        maxLength={6}
        style={{ ...inputStyle, letterSpacing: 6, textAlign: 'center', fontSize: 20 }}
      />
    </>
  );
}
