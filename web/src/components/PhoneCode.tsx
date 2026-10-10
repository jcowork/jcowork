import { useT } from '../i18n';

const inputStyle: React.CSSProperties = {
  width: '100%', padding: 10, marginBottom: 12, borderRadius: 8,
  border: '1px solid #555', background: '#1a1a1a', color: '#eee', fontSize: 16,
  boxSizing: 'border-box',
};

interface PhoneCodeFieldsProps {
  phone: string;
  code: string;
  onPhoneChange: (value: string) => void;
  onCodeChange: (value: string) => void;
  onSendCode: () => void;
  /** Seconds left on the resend cooldown; 0 means the button is live. */
  countdown: number;
  sending: boolean;
  autoFocus?: boolean;
}

/**
 * Phone number + "get code" button + code input.
 *
 * Shared by every screen that has to prove control of a number — SMS login,
 * the forced admin binding, and rebinding from Settings — because they all
 * collect the same two values against the same endpoint.
 */
export default function PhoneCodeFields({
  phone, code, onPhoneChange, onCodeChange, onSendCode, countdown, sending, autoFocus = true,
}: PhoneCodeFieldsProps) {
  const t = useT();
  const busy = sending || countdown > 0;
  return (
    <>
      <div style={{ display: 'flex', gap: 8, marginBottom: 12 }}>
        <input
          type="tel"
          inputMode="numeric"
          autoComplete="tel"
          placeholder={t('phoneNumber')}
          value={phone}
          onChange={(e) => onPhoneChange(e.target.value)}
          maxLength={20}
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
        placeholder={t('smsCode')}
        value={code}
        onChange={(e) => onCodeChange(e.target.value)}
        maxLength={6}
        style={{ ...inputStyle, letterSpacing: 6, textAlign: 'center', fontSize: 20 }}
      />
    </>
  );
}
