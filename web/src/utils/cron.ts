/**
 * Cron expression utilities for periodic task scheduling.
 * Uses 6-field format: second minute hour dom month dow
 */

export type TranslationFn = (key: string) => string;

/**
 * Parse a 6-field cron expression and return a human-readable frequency description.
 * @param schedule - The cron expression (e.g., "0 30 * * * *" or "0 0 15 * * 1,3,5")
 * @param t - Translation function for i18n
 * @returns Human-readable frequency string
 */
export function formatFrequency(
  schedule: string,
  t: TranslationFn
): string {
  const parts = schedule.split(' ');
  if (parts.length < 6) return schedule;
  
  const [sec, min, hr, dom, mon, dow] = parts;
  const timeStr = `${hr}:${min.padStart(2, '0')}:${sec.padStart(2, '0')}`;
  
  // Hourly: sec minute hour=* dom=* mon=*
  if (dom === '*' && mon === '*' && hr === '*') {
    return `${t('frequencyHourly')} (${min}分${sec}秒)`;
  }
  
  // Yearly: sec minute hour dom month *
  if (dom !== '*' && mon !== '*' && dow === '*') {
    const monthNames = ['monthJanuary', 'monthFebruary', 'monthMarch', 'monthApril', 'monthMay', 'monthJune',
                        'monthJuly', 'monthAugust', 'monthSeptember', 'monthOctober', 'monthNovember', 'monthDecember'];
    const monthIdx = parseInt(mon, 10) - 1;
    const monthLabel = t(monthNames[monthIdx] || mon);
    return `${t('frequencyYearly')} ${monthLabel}${dom}${t('dayOfMonth')} ${timeStr}`;
  }
  
  // Weekly: sec minute hour * * dayOfWeek(s)
  if (dom === '*' && mon === '*' && dow !== '*') {
    const dayNames = ['daySunday', 'dayMonday', 'dayTuesday', 'dayWednesday', 'dayThursday', 'dayFriday', 'daySaturday'];
    const days = dow.split(',').map(d => parseInt(d, 10));
    const dayLabels = days.map(d => t(dayNames[d] || String(d)));
    return `${t('frequencyWeekly')} ${dayLabels.join('/')} ${timeStr}`;
  }
  
  // Daily: sec minute hour * * *
  if (dom === '*' && mon === '*') {
    return `${t('frequencyDaily')} ${timeStr}`;
  }
  
  // Monthly: sec minute hour dom * *
  return `${t('frequencyMonthly')} ${dom}${t('dayOfMonth')} ${timeStr}`;
}

/**
 * Build a 6-field cron expression from frequency and time parameters.
 * Format: second minute hour dom month dow
 * @param frequency - 'hourly', 'daily', 'weekly', 'monthly', or 'yearly'
 * @param second - Second value (0-59)
 * @param minute - Minute value (0-59)
 * @param hour - Hour value (0-23)
 * @param day - Day of month (1-28), used for monthly/yearly
 * @param month - Month (1-12), used for yearly
 * @param daysOfWeek - Array of days (0-6, 0=Sun), used for weekly
 * @returns Cron expression string
 * @throws Error if parameters are invalid
 */
export function buildCronExpression(
  frequency: string,
  second?: number,
  minute?: number,
  hour?: number,
  day?: number,
  month?: number,
  daysOfWeek?: number[]
): string {
  const s = second ?? 0;
  const m = minute ?? 0;
  const h = hour ?? 9;
  
  // Validate second
  if (s > 59) {
    throw new Error(`Invalid second ${s} (must be 0-59)`);
  }
  
  // Validate minute for all frequencies except hourly
  if (frequency !== 'hourly') {
    if (m > 59) {
      throw new Error(`Invalid minute ${m} for ${frequency} (must be 0-59)`);
    }
    if (h > 23) {
      throw new Error(`Invalid hour ${h} for ${frequency} (must be 0-23)`);
    }
  }
  
  switch (frequency) {
    case 'hourly': {
      if (m > 59) {
        throw new Error(`Invalid minute ${m} for hourly (must be 0-59)`);
      }
      return `${s} ${m} * * * *`;
    }
    
    case 'daily': {
      return `${s} ${m} ${h} * * *`;
    }
    
    case 'weekly': {
      if (!daysOfWeek || daysOfWeek.length === 0) {
        throw new Error('days_of_week must not be empty for weekly');
      }
      // Validate, dedupe, and sort days
      const sortedDows = [...new Set(daysOfWeek)].sort((a, b) => a - b);
      for (const dow of sortedDows) {
        if (dow > 6) {
          throw new Error(`Invalid day_of_week ${dow} for weekly (must be 0-6)`);
        }
      }
      return `${s} ${m} ${h} * * ${sortedDows.join(',')}`;
    }
    
    case 'monthly': {
      const d = Math.min(day ?? 1, 28);
      return `${s} ${m} ${h} ${d} * *`;
    }
    
    case 'yearly': {
      const d = Math.min(day ?? 1, 28);
      const mo = Math.max(1, Math.min(month ?? 1, 12));
      return `${s} ${m} ${h} ${d} ${mo} *`;
    }
    
    default:
      throw new Error(`Invalid frequency '${frequency}'. Use: hourly, daily, weekly, monthly, yearly`);
  }
}
