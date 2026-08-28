/**
 * Tests for cron expression utilities.
 */

declare const process: { exit: (code: number) => void } | undefined;

import { buildCronExpression, formatFrequency } from './cron';

// Minimal test runner
let passed = 0;
let failed = 0;
let total = 0;

function assertEq(actual: string, expected: string, name: string) {
  total++;
  if (actual === expected) {
    passed++;
    console.log(`✓ ${name}`);
  } else {
    failed++;
    console.log(` ${name}`);
    console.log(`  Expected: "${expected}"`);
    console.log(`  Actual:   "${actual}"`);
  }
}

function assertThrows(fn: () => void, messageContains?: string) {
  try {
    fn();
    throw new Error('Expected function to throw');
  } catch (e) {
    if (messageContains && !(e as Error).message.includes(messageContains)) {
      throw new Error(`Expected error to contain "${messageContains}", got: ${(e as Error).message}`, { cause: e });
    }
  }
}

// Mock translation function
const t = (key: string) => {
  const translations: Record<string, string> = {
    frequencyHourly: '每小时',
    frequencyDaily: '每日',
    frequencyWeekly: '每周',
    frequencyMonthly: '每月',
    frequencyYearly: '每年',
    dayOfMonth: '日',
    daySunday: '日',
    dayMonday: '一',
    dayTuesday: '二',
    dayWednesday: '三',
    dayThursday: '四',
    dayFriday: '五',
    daySaturday: '六',
    monthJanuary: '1月',
    monthFebruary: '2月',
    monthMarch: '3月',
    monthApril: '4月',
    monthMay: '5月',
    monthJune: '6月',
    monthJuly: '7月',
    monthAugust: '8月',
    monthSeptember: '9月',
    monthOctober: '10月',
    monthNovember: '11月',
    monthDecember: '12月',
  };
  return translations[key] || key;
};

console.log('\n=== buildCronExpression tests ===\n');

// --- Hourly tests ---
assertEq(buildCronExpression('hourly'), '0 0 * * * *', 'hourly: default');
assertEq(buildCronExpression('hourly', undefined, 30), '0 30 * * * *', 'hourly: specific minute');
assertEq(buildCronExpression('hourly', undefined, 59), '0 59 * * * *', 'hourly: minute 59');
assertEq(buildCronExpression('hourly', undefined, 0, 10), '0 0 * * * *', 'hourly: ignores hour');
assertThrows(() => buildCronExpression('hourly', undefined, 60), 'Invalid minute');
assertThrows(() => buildCronExpression('hourly', 60), 'Invalid second');

// --- Daily tests ---
assertEq(buildCronExpression('daily'), '0 0 9 * * *', 'daily: default');
assertEq(buildCronExpression('daily', 0, 30, 15), '0 30 15 * * *', 'daily: specific time');
assertEq(buildCronExpression('daily', 0, 0, 0), '0 0 0 * * *', 'daily: midnight');
assertEq(buildCronExpression('daily', 0, 59, 23), '0 59 23 * * *', 'daily: end of day');
assertEq(buildCronExpression('daily', 45, 30, 15), '45 30 15 * * *', 'daily: with second');
assertThrows(() => buildCronExpression('daily', 0, 60, 10), 'Invalid minute');
assertThrows(() => buildCronExpression('daily', 0, 30, 24), 'Invalid hour');

// --- Weekly tests ---
assertEq(buildCronExpression('weekly', 0, 0, 9, undefined, undefined, [1]), '0 0 9 * * 1', 'weekly: default (Monday)');
assertEq(buildCronExpression('weekly', 0, 0, 10, undefined, undefined, [0]), '0 0 10 * * 0', 'weekly: Sunday');
assertEq(buildCronExpression('weekly', 0, 30, 14, undefined, undefined, [6]), '0 30 14 * * 6', 'weekly: Saturday');
assertEq(buildCronExpression('weekly', 0, 0, 9, undefined, undefined, [1, 3, 5]), '0 0 9 * * 1,3,5', 'weekly: Mon/Wed/Fri');
assertEq(buildCronExpression('weekly', 30, 0, 9, undefined, undefined, [1]), '30 0 9 * * 1', 'weekly: with second');
assertEq(buildCronExpression('weekly', 0, 0, 9, undefined, undefined, [1, 2, 3, 4, 5]), '0 0 9 * * 1,2,3,4,5', 'weekly: weekdays');
assertEq(buildCronExpression('weekly', 0, 0, 10, undefined, undefined, [0, 6]), '0 0 10 * * 0,6', 'weekly: weekends');
assertEq(buildCronExpression('weekly', 0, 0, 9, undefined, undefined, [0, 1, 2, 3, 4, 5, 6]), '0 0 9 * * 0,1,2,3,4,5,6', 'weekly: all days');
assertEq(buildCronExpression('weekly', 0, 0, 9, undefined, undefined, [1, 1, 3, 3]), '0 0 9 * * 1,3', 'weekly: dedup');
assertEq(buildCronExpression('weekly', 0, 0, 9, undefined, undefined, [5, 1, 3]), '0 0 9 * * 1,3,5', 'weekly: sort');
assertThrows(() => buildCronExpression('weekly', 0, 0, 9, undefined, undefined, [7]), 'Invalid day_of_week');
assertThrows(() => buildCronExpression('weekly', 0, 0, 9, undefined, undefined, []), 'must not be empty');

// --- Monthly tests ---
assertEq(buildCronExpression('monthly'), '0 0 9 1 * *', 'monthly: default');
assertEq(buildCronExpression('monthly', 0, 30, 14, 15), '0 30 14 15 * *', 'monthly: specific');
assertEq(buildCronExpression('monthly', 20, 30, 14, 15), '20 30 14 15 * *', 'monthly: with second');
assertEq(buildCronExpression('monthly', 0, 0, 10, 28), '0 0 10 28 * *', 'monthly: day 28');
assertEq(buildCronExpression('monthly', 0, 0, 10, 31), '0 0 10 28 * *', 'monthly: day clamped');

// --- Yearly tests ---
assertEq(buildCronExpression('yearly'), '0 0 9 1 1 *', 'yearly: default (Jan 1st)');
assertEq(buildCronExpression('yearly', 0, 30, 14, 15, 6), '0 30 14 15 6 *', 'yearly: June 15th');
assertEq(buildCronExpression('yearly', 45, 0, 10, 1, 3), '45 0 10 1 3 *', 'yearly: with second');
assertEq(buildCronExpression('yearly', 0, 0, 9, 1, 15), '0 0 9 1 12 *', 'yearly: month clamped');
assertEq(buildCronExpression('yearly', 0, 0, 9, 31, 6), '0 0 9 28 6 *', 'yearly: day clamped');

// --- Invalid frequency ---
assertThrows(() => buildCronExpression('biweekly'), 'Invalid frequency');
assertThrows(() => buildCronExpression(''), 'Invalid frequency');
assertThrows(() => buildCronExpression('weekdays'), 'Invalid frequency');
assertThrows(() => buildCronExpression('weekends'), 'Invalid frequency');

console.log('\n=== formatFrequency tests ===\n');

assertEq(formatFrequency('0 0 * * * *', t), '每小时 (0分0秒)', 'formatFrequency: hourly default');
assertEq(formatFrequency('0 30 * * * *', t), '每小时 (30分0秒)', 'formatFrequency: hourly at 30');
assertEq(formatFrequency('0 0 9 * * *', t), '每日 9:00:00', 'formatFrequency: daily default');
assertEq(formatFrequency('0 30 15 * * *', t), '每日 15:30:00', 'formatFrequency: daily at 15:30');
assertEq(formatFrequency('45 30 15 * * *', t), '每日 15:30:45', 'formatFrequency: daily with second');
assertEq(formatFrequency('0 0 9 1 * *', t), '每月 1日 9:00:00', 'formatFrequency: monthly default');
assertEq(formatFrequency('0 30 14 15 * *', t), '每月 15日 14:30:00', 'formatFrequency: monthly specific');
assertEq(formatFrequency('0 0 9 * * 1', t), '每周 一 9:00:00', 'formatFrequency: weekly Monday');
assertEq(formatFrequency('0 0 9 * * 0', t), '每周 日 9:00:00', 'formatFrequency: weekly Sunday');
assertEq(formatFrequency('0 0 9 * * 1,3,5', t), '每周 一/三/五 9:00:00', 'formatFrequency: weekly Mon/Wed/Fri');
assertEq(formatFrequency('0 0 9 * * 1,2,3,4,5', t), '每周 一/二/三/四/五 9:00:00', 'formatFrequency: weekly weekdays');
assertEq(formatFrequency('0 0 10 * * 0,6', t), '每周 日/六 10:00:00', 'formatFrequency: weekly weekends');
assertEq(formatFrequency('0 0 9 1 1 *', t), '每年 1月1日 9:00:00', 'formatFrequency: yearly default');
assertEq(formatFrequency('0 30 14 15 6 *', t), '每年 6月15日 14:30:00', 'formatFrequency: yearly June 15th');
assertEq(formatFrequency('45 0 10 1 3 *', t), '每年 3月1日 10:00:45', 'formatFrequency: yearly with second');
assertEq(formatFrequency('invalid', t), 'invalid', 'formatFrequency: invalid cron');

console.log('\n=== Summary ===\n');
console.log(`Passed: ${passed}`);
console.log(`Failed: ${failed}`);
console.log(`Total:  ${total}`);

if (failed > 0) {
  const processObj = typeof process !== 'undefined' ? process : undefined;
  if (processObj) processObj.exit(1);
}
