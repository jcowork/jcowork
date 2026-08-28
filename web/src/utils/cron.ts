/**
 * Cron expression utilities for periodic task scheduling.
 */

export type TranslationFn = (key: string) => string;

/**
 * Parse a 5-field cron expression and return a human-readable frequency description.
 * @param schedule - The cron expression (e.g., "30 * * * *" or "0 15 * * 1,3,5")
 * @param t - Translation function for i18n
 * @returns Human-readable frequency string
 */
export function formatFrequency(
  schedule: string,
  t: TranslationFn
): string {
  const parts = schedule.split(' ');
  if (parts.length < 5) return schedule;
  
  const [min, hr, dom, , dow] = parts;
  
  // Hourly: minute hour=* dom=*
  if (dom === '*' && hr === '*') {
    return `${t('frequencyHourly')} (${min}分)`;
  }
  
  // Weekly: minute hour * * dayOfWeek(s)
  if (dom === '*' && dow !== '*') {
    const dayNames = ['daySunday', 'dayMonday', 'dayTuesday', 'dayWednesday', 'dayThursday', 'dayFriday', 'daySaturday'];
    const days = dow.split(',').map(d => parseInt(d, 10));
    const dayLabels = days.map(d => t(dayNames[d] || String(d)));
    return `${t('frequencyWeekly')} ${dayLabels.join('/')} ${hr}:${min.padStart(2, '0')}`;
  }
  
  // Daily: minute hour dom=*
  if (dom === '*') {
    return `${t('frequencyDaily')} ${hr}:${min.padStart(2, '0')}`;
  }
  
  // Monthly: minute hour dom
  return `${t('frequencyMonthly')} ${dom}${t('dayOfMonth')} ${hr}:${min.padStart(2, '0')}`;
}

/**
 * Build a 5-field cron expression from frequency and time parameters.
 * @param frequency - 'hourly', 'daily', 'weekly', or 'monthly'
 * @param minute - Minute value (0-59)
 * @param hour - Hour value (0-23)
 * @param day - Day of month (1-28), used for monthly
 * @param daysOfWeek - Array of days (0-6, 0=Sun), used for weekly
 * @returns Cron expression string
 * @throws Error if parameters are invalid
 */
export function buildCronExpression(
  frequency: string,
  minute?: number,
  hour?: number,
  day?: number,
  daysOfWeek?: number[]
): string {
  const m = minute ?? 0;
  const h = hour ?? 9;
  
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
      return `${m} * * * *`;
    }
    
    case 'daily': {
      return `${m} ${h} * * *`;
    }
    
    case 'weekly': {
      const dows = daysOfWeek && daysOfWeek.length > 0 ? daysOfWeek : [1]; // Default: Monday
      // Validate, dedupe, and sort days
      const sortedDows = [...new Set(dows)].sort((a, b) => a - b);
      for (const dow of sortedDows) {
        if (dow > 6) {
          throw new Error(`Invalid day_of_week ${dow} for weekly (must be 0-6)`);
        }
      }
      if (sortedDows.length === 0) {
        throw new Error('days_of_week must not be empty for weekly');
      }
      return `${m} ${h} * * ${sortedDows.join(',')}`;
    }
    
    case 'monthly': {
      const d = Math.min(day ?? 1, 28);
      return `${m} ${h} ${d} * *`;
    }
    
    default:
      throw new Error(`Invalid frequency '${frequency}'. Use: hourly, daily, weekly, monthly`);
  }
}
