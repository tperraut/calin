// Builders for usage, issues and automations, shared by the test files. Never imported by the app.
import type { Automation } from "../gen/Automation";
import type { AutomationRun } from "../gen/AutomationRun";
import type { Issue } from "../gen/Issue";
import type { LimitWindow } from "../gen/LimitWindow";
import type { RunnerLimits } from "../gen/RunnerLimits";
import type { Usage } from "../gen/Usage";
import type { UsageRow } from "../gen/UsageRow";
import type { UsageTotals } from "../gen/UsageTotals";

export const totals = (cost: number, patch: Partial<UsageTotals> = {}): UsageTotals => ({
  cost_usd: cost,
  tokens_in: Math.round(cost * 1000),
  tokens_out: Math.round(cost * 100),
  sessions: cost > 0 ? 1 : 0,
  ...patch,
});

export const row = (key: string, cost: number, label = key): UsageRow => ({
  key,
  label,
  totals: totals(cost),
});

export function makeUsage(by: Usage["by"], rows: UsageRow[], total?: UsageTotals): Usage {
  const sum = rows.reduce(
    (all, one) => ({
      cost_usd: all.cost_usd + one.totals.cost_usd,
      tokens_in: all.tokens_in + one.totals.tokens_in,
      tokens_out: all.tokens_out + one.totals.tokens_out,
      sessions: all.sessions + one.totals.sessions,
    }),
    totals(0),
  );
  return { from: 0, to: 1, by, totals: total ?? sum, rows };
}

export const makeWindow = (name: string, patch: Partial<LimitWindow> = {}): LimitWindow => ({
  name,
  used: 0.5,
  resets_at: null,
  refused: false,
  ...patch,
});

export const makeLimits = (
  runner: string,
  windows: LimitWindow[],
  reportedAt = 1000,
): RunnerLimits => ({ runner, windows, reported_at: reportedAt });

export const makeIssue = (number: number, patch: Partial<Issue> = {}): Issue => ({
  number,
  title: `Issue ${number}`,
  state: "open",
  labels: [],
  author: "ada",
  updated_at: 1000,
  url: `https://github.com/acme/api/issues/${number}`,
  ...patch,
});

export const makeRun = (id: string, patch: Partial<AutomationRun> = {}): AutomationRun => ({
  id,
  at: 1000,
  cause: "schedule",
  outcome: "started",
  session_id: "s1",
  session_status: "running",
  detail: null,
  ...patch,
});

export const makeAutomation = (id: string, patch: Partial<Automation> = {}): Automation => ({
  id,
  name: `Automation ${id}`,
  enabled: true,
  trigger: { kind: "schedule", cron: "0 8 * * 1-5" },
  session: {
    prompt: "Check {{date}}",
    workflow: null,
    repos: [],
    runner: null,
    model: null,
    permission_mode: "accept_edits",
  },
  trigger_text: "Every weekday at 08:00",
  next_run_at: null,
  last_poll_at: null,
  last_poll_problem: null,
  last_run: null,
  disabled_reason: null,
  created_at: 1000,
  ...patch,
});
