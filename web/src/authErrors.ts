import type { TranslationKey } from './i18n';

type Translate = (key: TranslationKey) => string;

/** Extra numbers the SMS endpoints put alongside some of their errors. */
export interface AuthErrorDetail {
  retry_after?: number;
  attempts_left?: number;
}

/**
 * Turn a backend auth/profile error string into a localized message.
 *
 * The server reports prose rather than error codes, so this matches on
 * substrings. Shared by the login screens and the Settings profile card, which
 * hit the same code-verification endpoint and must not describe its failures
 * differently. Order matters: the more specific strings have to be tested
 * before the ones they contain.
 */
export function mapAuthError(t: Translate, raw: string, extra?: AuthErrorDetail): string {
  const wait = extra?.retry_after;
  if (raw.includes('Invalid phone number')) return t('invalidPhone');
  // Email-specific strings first: none of them contain the phone strings, but
  // keeping the channel order explicit avoids surprises when they drift.
  if (raw.includes('Invalid email address')) return t('invalidEmail');
  if (raw.includes('Invalid email or password')) return t('invalidEmailOrPassword');
  if (raw.includes('Email address is already registered')) return t('emailAlreadyRegistered');
  if (raw.includes('Daily limit reached for this email address')) return t('emailDailyLimit');
  if (raw.includes('Email quota exhausted')) return t('emailQuotaExhausted');
  if (raw.includes('Email registration is disabled')) return t('emailRegisterDisabled');
  if (raw.includes('Password must be at least')) return t('passwordMinLength');
  if (raw.includes('Password must be at most')) return t('passwordMaxLength');
  if (raw.includes('Password cannot be only whitespace')) return t('passwordMinLength');
  if (raw.includes('Please wait before requesting another code')) {
    return wait ? t('resendWait').replace('{n}', String(wait)) : t('resendTooSoon');
  }
  if (raw.includes('Daily limit reached for this phone number')) return t('smsDailyLimit');
  if (raw.includes('SMS quota exhausted')) return t('smsQuotaExhausted');
  if (raw.includes('Failed to send verification code')) return t('smsSendFailed');
  if (raw.includes('Verification code is required')) return t('codeRequired');
  if (raw.includes('Verification code has expired')) return t('codeExpired');
  if (raw.includes('Invalid verification code')) {
    const left = extra?.attempts_left;
    return left !== undefined && left > 0
      ? t('codeWrongAttemptsLeft').replace('{n}', String(left))
      : t('codeWrong');
  }
  if (raw.includes('Too many failed attempts')) {
    return wait ? t('tooManyAttemptsWait').replace('{n}', String(wait)) : t('tooManyAttempts');
  }
  if (raw.includes('Phone number is not registered')) return t('phoneNotRegistered');
  if (raw.includes('Account has been deleted')) return t('accountDeletedMessage');
  if (raw.includes('Invalid username or password')) return t('authFailed');
  if (raw.includes('already bound')) return t('phoneAlreadyBound');
  if (raw.includes('Username must be between') || raw.includes('Username cannot contain')) {
    return t('displayNameHint');
  }
  if (raw.includes('already exists')) return t('nameTaken');
  if (raw.includes('Account not found')) return t('userNotFound');
  return raw || t('operationFailed');
}
