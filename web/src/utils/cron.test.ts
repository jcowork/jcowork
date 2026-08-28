/**
 * Tests for cron utilities.
 * Run with: node --experimental-strip-types cron.test.ts
 * Or: npx tsx cron.test.ts
 */

// Declare process for Node.js environment
declare const process: { exit: (code: number) => void } | undefined;

import { formatFrequency, buildCronExpression } from './cron';

// Simple test runner
let passed = 0;
let failed = 0;

function test(name: string, fn: () => void) {
  try {
    fn();
    console.log(`✓ ${name}`);
    passed++;
  } catch (e) {
    console.error(`✗ ${name}`);
    console.error(`  ${e}`);
    failed++;
  }
}

function assertEqual(actual: unknown, expected: unknown, message?: string) {
  if (actual !== expected) {
    throw new Error(
      `${message || 'Assertion failed'}: expected ${JSON.stringify(expected)}, got ${JSON.stringify(actual)}`
    );
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
    dayOfMonth: '日',
    daySunday: '日',
    dayMonday: '一',
    dayTuesday: '二',
    dayWednesday: '三',
    dayThursday: '四',
    dayFriday: '五',
    daySaturday: '六',
  };
  return translations[key] || key;
};

console.log('\n=== buildCronExpression tests ===\n');

// Hourly tests
test('hourly: default (minute=0)', () => {
  assertEqual(buildCronExpression('hourly'), '0 * * * *');
});

test('hourly: specific minute (30)', () => {
  assertEqual(buildCronExpression('hourly', 30), '30 * * * *');
});

test('hourly: minute 59 (boundary)', () => {
  assertEqual(buildCronExpression('hourly', 59), '59 * * * *');
});

test('hourly: invalid minute (60)', () => {
  assertThrows(() => buildCronExpression('hourly', 60), 'Invalid minute');
});

test('hourly: ignores hour and day params', () => {
  assertEqual(buildCronExpression('hourly', 15, 10, 5, [3]), '15 * * * *');
});

// Daily tests
test('daily: default (9:00)', () => {
  assertEqual(buildCronExpression('daily'), '0 9 * * *');
});

test('daily: specific time (15:30)', () => {
  assertEqual(buildCronExpression('daily', 30, 15), '30 15 * * *');
});

test('daily: midnight (0:00)', () => {
  assertEqual(buildCronExpression('daily', 0, 0), '0 0 * * *');
});

test('daily: end of day (23:59)', () => {
  assertEqual(buildCronExpression('daily', 59, 23), '59 23 * * *');
});

test('daily: invalid minute (60)', () => {
  assertThrows(() => buildCronExpression('daily', 60, 10), 'Invalid minute');
});

test('daily: invalid hour (24)', () => {
  assertThrows(() => buildCronExpression('daily', 30, 24), 'Invalid hour');
});

test('daily: ignores day param', () => {
  assertEqual(buildCronExpression('daily', 0, 12, 15, [3]), '0 12 * * *');
});

// Weekly tests (multi-select)
test('weekly: default (Monday at 9:00)', () => {
  assertEqual(buildCronExpression('weekly'), '0 9 * * 1');
});

test('weekly: single day Sunday', () => {
  assertEqual(buildCronExpression('weekly', 0, 10, undefined, [0]), '0 10 * * 0');
});

test('weekly: single day Saturday', () => {
  assertEqual(buildCronExpression('weekly', 30, 14, undefined, [6]), '30 14 * * 6');
});

test('weekly: multiple days Mon/Wed/Fri', () => {
  assertEqual(buildCronExpression('weekly', 0, 9, undefined, [1, 3, 5]), '0 9 * * 1,3,5');
});

test('weekly: weekdays equivalent (Mon-Fri)', () => {
  assertEqual(buildCronExpression('weekly', 0, 9, undefined, [1, 2, 3, 4, 5]), '0 9 * * 1,2,3,4,5');
});

test('weekly: weekends equivalent (Sat/Sun)', () => {
  assertEqual(buildCronExpression('weekly', 0, 10, undefined, [0, 6]), '0 10 * * 0,6');
});

test('weekly: all days', () => {
  assertEqual(buildCronExpression('weekly', 0, 9, undefined, [0, 1, 2, 3, 4, 5, 6]), '0 9 * * 0,1,2,3,4,5,6');
});

test('weekly: duplicate days deduped', () => {
  assertEqual(buildCronExpression('weekly', 0, 9, undefined, [1, 1, 3, 3]), '0 9 * * 1,3');
});

test('weekly: unsorted days sorted', () => {
  assertEqual(buildCronExpression('weekly', 0, 9, undefined, [5, 1, 3]), '0 9 * * 1,3,5');
});

test('weekly: invalid day_of_week (7)', () => {
  assertThrows(() => buildCronExpression('weekly', 0, 9, undefined, [7]), 'Invalid day_of_week');
});

test('weekly: invalid minute (60)', () => {
  assertThrows(() => buildCronExpression('weekly', 60, 9, undefined, [1]), 'Invalid minute');
});

test('weekly: invalid hour (24)', () => {
  assertThrows(() => buildCronExpression('weekly', 0, 24, undefined, [1]), 'Invalid hour');
});

// Monthly tests
test('monthly: default (1st at 9:00)', () => {
  assertEqual(buildCronExpression('monthly'), '0 9 1 * *');
});

test('monthly: specific time (15th at 14:30)', () => {
  assertEqual(buildCronExpression('monthly', 30, 14, 15), '30 14 15 * *');
});

test('monthly: day 28 (max allowed)', () => {
  assertEqual(buildCronExpression('monthly', 0, 10, 28), '0 10 28 * *');
});

test('monthly: day > 28 clamped to 28', () => {
  assertEqual(buildCronExpression('monthly', 0, 10, 31), '0 10 28 * *');
});

test('monthly: invalid minute (100)', () => {
  assertThrows(() => buildCronExpression('monthly', 100, 10, 15), 'Invalid minute');
});

test('monthly: invalid hour (25)', () => {
  assertThrows(() => buildCronExpression('monthly', 0, 25, 15), 'Invalid hour');
});

// Invalid frequency tests
test('invalid frequency: yearly', () => {
  assertThrows(() => buildCronExpression('yearly', 0, 9), 'Invalid frequency');
});

test('invalid frequency: weekdays (removed)', () => {
  assertThrows(() => buildCronExpression('weekdays', 0, 9), 'Invalid frequency');
});

test('invalid frequency: weekends (removed)', () => {
  assertThrows(() => buildCronExpression('weekends', 0, 9), 'Invalid frequency');
});

test('invalid frequency: empty string', () => {
  assertThrows(() => buildCronExpression(''), 'Invalid frequency');
});

console.log('\n=== formatFrequency tests ===\n');

// Hourly display tests
test('formatFrequency: hourly at minute 0', () => {
  assertEqual(formatFrequency('0 * * * *', t), '每小时 (0分)');
});

test('formatFrequency: hourly at minute 30', () => {
  assertEqual(formatFrequency('30 * * * *', t), '每小时 (30分)');
});

// Daily display tests
test('formatFrequency: daily at 9:00', () => {
  assertEqual(formatFrequency('0 9 * * *', t), '每日 9:00');
});

test('formatFrequency: daily at 15:30', () => {
  assertEqual(formatFrequency('30 15 * * *', t), '每日 15:30');
});

// Monthly display tests
test('formatFrequency: monthly on 1st at 9:00', () => {
  assertEqual(formatFrequency('0 9 1 * *', t), '每月 1日 9:00');
});

test('formatFrequency: monthly on 15th at 14:30', () => {
  assertEqual(formatFrequency('30 14 15 * *', t), '每月 15日 14:30');
});

// Weekly display tests (multi-select)
test('formatFrequency: weekly on Monday', () => {
  assertEqual(formatFrequency('0 9 * * 1', t), '每周 一 9:00');
});

test('formatFrequency: weekly on Sunday', () => {
  assertEqual(formatFrequency('0 10 * * 0', t), '每周 日 10:00');
});

test('formatFrequency: weekly on Mon/Wed/Fri', () => {
  assertEqual(formatFrequency('0 9 * * 1,3,5', t), '每周 一/三/五 9:00');
});

test('formatFrequency: weekly weekdays equivalent', () => {
  assertEqual(formatFrequency('0 9 * * 1,2,3,4,5', t), '每周 一/二/三/四/五 9:00');
});

test('formatFrequency: weekly weekends equivalent', () => {
  assertEqual(formatFrequency('0 10 * * 0,6', t), '每周 日/六 10:00');
});

test('formatFrequency: weekly every day', () => {
  assertEqual(formatFrequency('0 9 * * 0,1,2,3,4,5,6', t), '每周 日/一/二/三/四/五/六 9:00');
});

// Edge cases
test('formatFrequency: invalid cron (less than 5 fields)', () => {
  assertEqual(formatFrequency('* * *', t), '* * *');
});

test('formatFrequency: handles minute padding correctly', () => {
  assertEqual(formatFrequency('0 15 * * *', t), '每日 15:00');
  assertEqual(formatFrequency('0 9 1 * *', t), '每月 1日 9:00');
});

// Summary
console.log('\n=== Summary ===\n');
console.log(`Passed: ${passed}`);
console.log(`Failed: ${failed}`);
console.log(`Total: ${passed + failed}`);

if (failed > 0) {
  // Exit with error code for CI
  if (typeof process !== 'undefined') {
    process.exit(1);
  }
}
