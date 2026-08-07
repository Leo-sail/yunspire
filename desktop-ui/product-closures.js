const REPORT_PERIODS = new Set(['daily', 'weekly', 'monthly', 'annual']);
const REPORT_SECTIONS = new Set([
  'task_outcomes',
  'new_knowledge',
  'knowledge_health',
  'open_reviews',
  'skill_quality',
  'goals',
  'growth',
  'next_actions',
  'costs',
]);
const DELIVERY_TYPES = new Set(['local_notification', 'wechat', 'feishu', 'approved_channel']);
const APPROVAL_POLICIES = new Set(['local_auto_external_review', 'preapproved_destinations', 'always_review']);
const REPORT_SUBSCRIPTION_STATES = new Set(['running', 'awaiting_approval', 'succeeded', 'failed', 'cancelled', 'skipped']);

export const DEFAULT_SHORTCUTS = Object.freeze({
  search: 'Primary+K',
  newNote: 'Primary+N',
  capture: 'Primary+P',
  scheduledCapture: 'Primary+Shift+P',
  assistant: 'Primary+Shift+A',
});

export function upsertRecordById(records, record) {
  if (!record?.id) throw new TypeError('待写入记录缺少 id');
  return [record, ...(Array.isArray(records) ? records : []).filter((item) => item?.id !== record.id)];
}

function clampInteger(value, minimum, maximum, fallback) {
  const number = Number(value);
  return Number.isInteger(number) ? Math.max(minimum, Math.min(maximum, number)) : fallback;
}

function validTimezone(value, fallback = 'UTC') {
  const requested = String(value || '').trim();
  try {
    if (requested) new Intl.DateTimeFormat('en-US', { timeZone: requested }).format(new Date());
    return requested || fallback;
  } catch {
    return fallback;
  }
}

function zonedParts(date, timeZone) {
  const parts = new Intl.DateTimeFormat('en-CA', {
    timeZone,
    year: 'numeric',
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
    hourCycle: 'h23',
  }).formatToParts(date);
  return Object.fromEntries(parts.filter((part) => part.type !== 'literal').map((part) => [part.type, Number(part.value)]));
}

function zonedTimeToUtc(year, month, day, hour, minute, second, timeZone) {
  const targetUtc = Date.UTC(year, month - 1, day, hour, minute, second, 0);
  let guess = targetUtc;
  for (let iteration = 0; iteration < 6; iteration += 1) {
    const parts = zonedParts(new Date(guess), timeZone);
    const representedUtc = Date.UTC(parts.year, parts.month - 1, parts.day, parts.hour, parts.minute, parts.second, 0);
    const nextGuess = guess - (representedUtc - targetUtc);
    if (nextGuess === guess) break;
    guess = nextGuess;
  }
  return new Date(guess);
}

function utcCalendarDate(parts) {
  return new Date(Date.UTC(parts.year, parts.month - 1, parts.day));
}

function calendarValues(date) {
  return { year: date.getUTCFullYear(), month: date.getUTCMonth() + 1, day: date.getUTCDate() };
}

function daysInMonth(year, month) {
  return new Date(Date.UTC(year, month, 0)).getUTCDate();
}

function normalizedTime(value, fallback = '20:00') {
  const match = String(value || '').trim().match(/^([01]?\d|2[0-3]):([0-5]\d)$/u);
  if (!match) return fallback;
  return `${match[1].padStart(2, '0')}:${match[2]}`;
}

export function reportPeriodRange(period = 'weekly', at = new Date(), options = {}) {
  const normalizedPeriod = REPORT_PERIODS.has(period) ? period : 'weekly';
  const generatedAt = at instanceof Date ? new Date(at) : new Date(at);
  if (!Number.isFinite(generatedAt.getTime())) throw new TypeError('报告生成时间无效');
  const timeZone = validTimezone(options.timeZone || options.timezone, 'UTC');
  const weekStart = options.weekStart === 'sunday' ? 'sunday' : 'monday';
  const current = zonedParts(generatedAt, timeZone);
  const currentCalendar = utcCalendarDate(current);
  let startCalendar = new Date(currentCalendar);
  if (normalizedPeriod === 'weekly') {
    const weekday = currentCalendar.getUTCDay();
    const offset = weekStart === 'sunday' ? weekday : (weekday + 6) % 7;
    startCalendar.setUTCDate(startCalendar.getUTCDate() - offset);
  } else if (normalizedPeriod === 'monthly') {
    startCalendar = new Date(Date.UTC(current.year, current.month - 1, 1));
  } else if (normalizedPeriod === 'annual') {
    startCalendar = new Date(Date.UTC(current.year, 0, 1));
  }
  const start = calendarValues(startCalendar);
  return {
    period: normalizedPeriod,
    timeZone,
    weekStart,
    start: zonedTimeToUtc(start.year, start.month, start.day, 0, 0, 0, timeZone),
    end: generatedAt,
  };
}

export function timestampForReportRecord(record) {
  const candidates = [
    record?.completedAt,
    record?.committedAt,
    record?.updatedAt,
    record?.createdAt,
    record?.occurredAt,
    record?.timestamp,
    record?.date,
  ];
  for (const value of candidates) {
    const parsed = Date.parse(String(value || ''));
    if (Number.isFinite(parsed)) return parsed;
  }
  return Number.NaN;
}

export function recordsInReportRange(records, range) {
  const start = range?.start instanceof Date ? range.start.getTime() : new Date(range?.start || 0).getTime();
  const end = range?.end instanceof Date ? range.end.getTime() : new Date(range?.end || 0).getTime();
  return (Array.isArray(records) ? records : []).filter((record) => {
    const timestamp = timestampForReportRecord(record);
    return Number.isFinite(timestamp) && timestamp >= start && timestamp <= end;
  });
}

export function computeReportSubscriptionNextRun(subscription, from = new Date()) {
  const currentTime = from instanceof Date ? new Date(from) : new Date(from);
  if (!Number.isFinite(currentTime.getTime())) throw new TypeError('订阅基准时间无效');
  const period = REPORT_PERIODS.has(subscription?.period) ? subscription.period : 'weekly';
  const timeZone = validTimezone(subscription?.timezone, 'UTC');
  const [hour, minute] = normalizedTime(subscription?.delivery_time || subscription?.runTime || '20:00').split(':').map(Number);
  const current = zonedParts(currentTime, timeZone);
  const calendarStart = Date.UTC(current.year, current.month - 1, current.day);
  const weekday = clampInteger(subscription?.weekday, 1, 7, 1);
  const dayOfMonth = clampInteger(subscription?.day_of_month ?? subscription?.dayOfMonth, 1, 31, 1);
  const annualMonth = clampInteger(subscription?.annual_month ?? subscription?.annualMonth, 1, 12, 1);
  const annualDay = clampInteger(subscription?.annual_day ?? subscription?.annualDay, 1, 31, 1);
  for (let offset = 0; offset < 800; offset += 1) {
    const calendar = new Date(calendarStart + offset * 86_400_000);
    const values = calendarValues(calendar);
    const candidateWeekday = calendar.getUTCDay() || 7;
    if (period === 'weekly' && candidateWeekday !== weekday) continue;
    if (period === 'monthly' && values.day !== Math.min(dayOfMonth, daysInMonth(values.year, values.month))) continue;
    if (period === 'annual') {
      const expectedDay = Math.min(annualDay, daysInMonth(values.year, annualMonth));
      if (values.month !== annualMonth || values.day !== expectedDay) continue;
    }
    const candidate = zonedTimeToUtc(values.year, values.month, values.day, hour, minute, 0, timeZone);
    if (candidate > currentTime) return candidate.toISOString();
  }
  throw new Error('无法计算报告订阅的下次运行时间');
}

export function parseReportScheduleText(message, fallback = {}) {
  const source = String(message || '').replace(/\s+/gu, ' ').trim();
  const weekdayNames = new Map([
    ['一', 1], ['二', 2], ['三', 3], ['四', 4], ['五', 5], ['六', 6], ['日', 7], ['天', 7],
  ]);
  const weekdayMatch = source.match(/(?:每周|星期|周)([一二三四五六日天])/u);
  const monthDayMatch = source.match(/每月(?:的)?\s*(\d{1,2})\s*[日号]/u);
  const annualMatch = source.match(/每年(?:的)?\s*(\d{1,2})\s*月\s*(\d{1,2})\s*[日号]?/u);
  let deliveryTime = normalizedTime(fallback.delivery_time || fallback.runTime || '', '');
  const colonTime = source.match(/(?:^|\D)([01]?\d|2[0-3]):([0-5]\d)(?:\D|$)/u);
  const chineseTime = source.match(/(凌晨|早上|上午|中午|下午|傍晚|晚上)?\s*(\d{1,2})\s*点(?:\s*(\d{1,2})\s*分?)?/u);
  if (colonTime) deliveryTime = normalizedTime(`${colonTime[1]}:${colonTime[2]}`);
  else if (chineseTime) {
    let hour = clampInteger(chineseTime[2], 0, 23, 0);
    const qualifier = chineseTime[1] || '';
    if (/下午|傍晚|晚上/u.test(qualifier) && hour < 12) hour += 12;
    if (/凌晨/u.test(qualifier) && hour === 12) hour = 0;
    if (/中午/u.test(qualifier) && hour < 11) hour += 12;
    deliveryTime = `${String(hour).padStart(2, '0')}:${String(clampInteger(chineseTime[3], 0, 59, 0)).padStart(2, '0')}`;
  }
  return {
    weekday: weekdayMatch ? weekdayNames.get(weekdayMatch[1]) : clampInteger(fallback.weekday, 1, 7, 1),
    dayOfMonth: monthDayMatch ? clampInteger(monthDayMatch[1], 1, 31, 1) : clampInteger(fallback.day_of_month ?? fallback.dayOfMonth, 1, 31, 1),
    annualMonth: annualMatch ? clampInteger(annualMatch[1], 1, 12, 1) : clampInteger(fallback.annual_month ?? fallback.annualMonth, 1, 12, 1),
    annualDay: annualMatch ? clampInteger(annualMatch[2], 1, 31, 1) : clampInteger(fallback.annual_day ?? fallback.annualDay, 1, 31, 1),
    deliveryTime: deliveryTime || (fallback.period === 'daily' ? '20:00' : '09:00'),
  };
}

function subscriptionId(value) {
  const normalized = String(value || '').toLowerCase().replace(/[^a-z0-9-]+/gu, '-').replace(/^-+|-+$/gu, '');
  return /^[a-z][a-z0-9-]*$/u.test(normalized) ? normalized : 'report-subscription';
}

function normalizedDeliveries(value) {
  return (Array.isArray(value) ? value : []).flatMap((item) => {
    const type = String(item?.type || '').trim();
    const destinationRef = String(item?.destination_ref || item?.destinationRef || '').trim();
    if (!DELIVERY_TYPES.has(type) || !destinationRef) return [];
    return [{ type, destination_ref: destinationRef }];
  }).filter((item, index, items) => items.findIndex((candidate) => candidate.type === item.type && candidate.destination_ref === item.destination_ref) === index);
}

export function normalizeReportSubscription(input = {}, options = {}) {
  const now = options.now instanceof Date ? options.now : new Date(options.now || Date.now());
  const period = REPORT_PERIODS.has(input.period) ? input.period : 'weekly';
  const fallbackTime = period === 'daily' ? '20:00' : '09:00';
  const deliveryTime = normalizedTime(input.delivery_time || input.runTime, fallbackTime);
  const timezone = validTimezone(input.timezone, options.timezone || 'UTC');
  const localDestination = String(input.local_destination || input.path || `60 Reviews/${period}`).replace(/^\/+|\.\./gu, '').trim();
  const sections = [...new Set((Array.isArray(input.sections) ? input.sections : ['task_outcomes', 'new_knowledge', 'open_reviews', 'growth', 'next_actions'])
    .map(String).filter((item) => REPORT_SECTIONS.has(item)))];
  const delivery = normalizedDeliveries(input.delivery);
  const approval = APPROVAL_POLICIES.has(input.approval) ? input.approval : 'local_auto_external_review';
  const weekday = clampInteger(input.weekday, 1, 7, 1);
  const dayOfMonth = clampInteger(input.day_of_month ?? input.dayOfMonth, 1, 31, 1);
  const annualMonth = clampInteger(input.annual_month ?? input.annualMonth, 1, 12, 1);
  const annualDay = clampInteger(input.annual_day ?? input.annualDay, 1, 31, 1);
  const createdAt = String(input.created_at || input.createdAt || now.toISOString());
  const updatedAt = String(input.updated_at || input.updatedAt || now.toISOString());
  const result = {
    id: subscriptionId(input.id),
    revision: Math.max(1, Number.isInteger(Number(input.revision)) ? Number(input.revision) : 1),
    name: String(input.name || '定期报告').trim().slice(0, 120) || '定期报告',
    period,
    enabled: input.enabled !== false,
    timezone,
    delivery_time: deliveryTime,
    week_start: input.week_start === 'sunday' ? 'sunday' : 'monday',
    weekday,
    day_of_month: dayOfMonth,
    annual_month: annualMonth,
    annual_day: annualDay,
    sections: sections.length ? sections : ['task_outcomes'],
    local_destination: /^(?:\.yunspire\/reports\/|60 Reviews\/)/u.test(localDestination) ? localDestination : `60 Reviews/${period}`,
    delivery,
    approval,
    quiet_hours: input.quiet_hours || null,
    vaultId: String(input.vaultId || input.vault_id || ''),
    vaultName: String(input.vaultName || input.vault_name || '本地 Obsidian'),
    creator: input.creator === 'user' ? 'user' : 'assistant',
    policy: String(input.policy || '失败后保留任务与日志，并按指数退避自动重试'),
    nextRun: input.nextRun || input.next_run || null,
    lastRunAt: input.lastRunAt || input.last_run_at || null,
    lastSkippedAt: input.lastSkippedAt || input.last_skipped_at || null,
    lastState: REPORT_SUBSCRIPTION_STATES.has(input.lastState || input.last_state) ? (input.lastState || input.last_state) : null,
    lastError: input.lastError || input.last_error || '',
    lastScheduledFor: input.lastScheduledFor || input.last_scheduled_for || null,
    lastOccurrenceId: input.lastOccurrenceId || input.last_occurrence_id || null,
    retryAttempt: Math.max(0, Math.trunc(Number(input.retryAttempt || input.retry_attempt || 0))),
    createdAt,
    updatedAt,
  };
  if (!result.nextRun && result.enabled) result.nextRun = computeReportSubscriptionNextRun(result, now);
  return result;
}

function normalizedPrimaryModifier(value) {
  return /^(?:meta|control|ctrl|primary)$/iu.test(value) ? 'Primary' : value;
}

export function normalizeShortcut(value) {
  const parts = String(value || '').split('+').map((part) => part.trim()).filter(Boolean);
  const modifiers = new Set();
  let key = '';
  parts.forEach((part) => {
    const normalized = normalizedPrimaryModifier(part);
    if (/^primary$/iu.test(normalized)) modifiers.add('Primary');
    else if (/^shift$/iu.test(normalized)) modifiers.add('Shift');
    else if (/^(?:alt|option)$/iu.test(normalized)) modifiers.add('Alt');
    else key = part.length === 1 ? part.toUpperCase() : part;
  });
  if (!key) return '';
  return [...['Primary', 'Shift', 'Alt'].filter((modifier) => modifiers.has(modifier)), key].join('+');
}

export function shortcutFromKeyboardEvent(event) {
  if (!event || ['Meta', 'Control', 'Shift', 'Alt'].includes(event.key)) return '';
  const parts = [];
  if (event.metaKey || event.ctrlKey) parts.push('Primary');
  if (event.shiftKey) parts.push('Shift');
  if (event.altKey) parts.push('Alt');
  const key = event.key.length === 1 ? event.key.toUpperCase() : event.key;
  if (!parts.length || ['Escape', 'Tab', 'Enter'].includes(key)) return '';
  return normalizeShortcut([...parts, key].join('+'));
}

export function shortcutMatchesEvent(event, binding) {
  const normalized = normalizeShortcut(binding);
  if (!normalized) return false;
  const parts = normalized.split('+');
  const key = parts.at(-1);
  const actualKey = event.key.length === 1 ? event.key.toUpperCase() : event.key;
  return actualKey === key
    && Boolean(event.metaKey || event.ctrlKey) === parts.includes('Primary')
    && Boolean(event.shiftKey) === parts.includes('Shift')
    && Boolean(event.altKey) === parts.includes('Alt');
}

export function shortcutActionFromEvent(event, shortcuts = {}) {
  const configured = { ...DEFAULT_SHORTCUTS, ...(shortcuts || {}) };
  return Object.keys(DEFAULT_SHORTCUTS).find((action) => shortcutMatchesEvent(event, configured[action])) || '';
}

export function shortcutConflicts(shortcuts) {
  const byBinding = new Map();
  Object.entries({ ...DEFAULT_SHORTCUTS, ...(shortcuts || {}) }).forEach(([action, value]) => {
    const binding = normalizeShortcut(value);
    if (!binding) return;
    if (!byBinding.has(binding)) byBinding.set(binding, []);
    byBinding.get(binding).push(action);
  });
  return new Map([...byBinding.entries()].filter(([, actions]) => actions.length > 1));
}

export function formatShortcut(value, platform = '') {
  const isMac = /mac|iphone|ipad/iu.test(platform);
  return normalizeShortcut(value).split('+').map((part) => {
    if (part === 'Primary') return isMac ? '⌘' : 'Ctrl';
    if (part === 'Shift') return isMac ? '⇧' : 'Shift';
    if (part === 'Alt') return isMac ? '⌥' : 'Alt';
    return part;
  }).join(isMac ? ' ' : '+');
}
