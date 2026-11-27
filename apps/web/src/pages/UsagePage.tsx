import { useSearchParams } from "@solidjs/router";
import {
  createEffect,
  createMemo,
  createSignal,
  For,
  Match,
  onSettled,
  Show,
  Switch,
  useContext,
} from "solid-js";
import { DayChart } from "../components/DayChart";
import { LimitMeters } from "../components/LimitMeters";
import type { Usage } from "../gen/Usage";
import type { UsageRow } from "../gen/UsageRow";
import { errorMessage } from "../lib/errors";
import { compactCount, formatCost } from "../lib/format";
import {
  fillDays,
  rangeBounds,
  rangeFromQuery,
  todayKey,
  USAGE_RANGES,
  type UsageRange,
} from "../lib/usage";
import { sessionPath } from "../panels/registry";
import { AppContext } from "../state/app";

/** integrations spec, section 1: `by=session` returns at most this many. */
export const SESSION_CAP = 200;

type Breakdowns = { day: Usage; session: Usage; repo: Usage; model: Usage };

type Load =
  | { status: "loading" }
  | { status: "ready"; data: Breakdowns; endsNow: boolean }
  | { status: "error"; message: string };

const numberFormat = new Intl.NumberFormat();

function Tokens(props: { count: number }) {
  return <span title={numberFormat.format(props.count)}>{compactCount(props.count)}</span>;
}

/** A breakdown by repo or by model: the share of the cost drawn as a thin bar, the numbers as text. */
function ShareTable(props: {
  caption: string;
  heading: string;
  rows: readonly UsageRow[];
  emptyLabel: string;
}) {
  // Each bar is the row's share of the total: equal rows read as equal halves, not as two full bars.
  const total = (): number => props.rows.reduce((sum, row) => sum + row.totals.cost_usd, 0);
  return (
    <section class="flex min-w-0 flex-col gap-2">
      <h2 class="font-semibold">{props.caption}</h2>
      <Show
        when={props.rows.length > 0}
        fallback={<p class="text-sm text-muted">Nothing in this range.</p>}
      >
        <table class="w-full text-sm" data-part={`by-${props.heading.toLowerCase()}`}>
          <caption class="sr-only">{props.caption}</caption>
          <thead class="text-left text-xs text-muted">
            <tr>
              <th scope="col" class="py-1 font-medium">
                {props.heading}
              </th>
              <th scope="col" class="py-1 text-right font-medium">
                Cost
              </th>
              <th scope="col" class="py-1 text-right font-medium whitespace-nowrap">
                Tokens in / out
              </th>
            </tr>
          </thead>
          <tbody class="divide-y divide-line">
            <For each={props.rows} keyed={(row) => row.key}>
              {(row) => (
                <tr>
                  <th scope="row" class="w-full max-w-0 py-1.5 pr-2 text-left font-normal">
                    <span class="block truncate">
                      {row().key === "" ? props.emptyLabel : row().label || row().key}
                    </span>
                    <span aria-hidden="true" class="mt-1 block h-1 rounded-full bg-sunken">
                      <span
                        class="block h-full rounded-full bg-accent"
                        style={{
                          width: `${total() > 0 ? (row().totals.cost_usd / total()) * 100 : 0}%`,
                        }}
                      />
                    </span>
                  </th>
                  <td class="py-1.5 text-right font-mono whitespace-nowrap">
                    {formatCost(row().totals.cost_usd)}
                  </td>
                  <td class="py-1.5 pl-2 text-right font-mono whitespace-nowrap">
                    <Tokens count={row().totals.tokens_in} /> /{" "}
                    <Tokens count={row().totals.tokens_out} />
                  </td>
                </tr>
              )}
            </For>
          </tbody>
        </table>
      </Show>
    </section>
  );
}

/** `/usage`: what sessions cost over a range, and how close the plan's limits are. */
export default function UsagePage() {
  const app = useContext(AppContext);
  const [search, setSearch] = useSearchParams<{ days: string }>();
  const range = (): UsageRange => rangeFromQuery(search.days);
  const [load, setLoad] = createSignal<Load>({ status: "loading" });
  const [attempt, setAttempt] = createSignal(0);
  let asked = 0;

  onSettled(() => app.loadLimits());

  createEffect(
    () => [range(), attempt()] as const,
    ([days]) => {
      asked += 1;
      const mine = asked;
      setLoad({ status: "loading" });
      const now = Date.now();
      const bounds = rangeBounds(days, now);
      // Every range offered ends now; one that does not has no today to mark.
      const endsNow = bounds.to >= now;
      const ask = (by: Usage["by"]) => app.usage({ ...bounds, by });
      Promise.all([ask("day"), ask("session"), ask("repo"), ask("model")]).then(
        ([day, session, repo, model]) => {
          if (mine === asked)
            setLoad({ status: "ready", data: { day, session, repo, model }, endsNow });
        },
        (error: unknown) => {
          if (mine === asked) setLoad({ status: "error", message: errorMessage(error) });
        },
      );
    },
  );

  const data = (): Breakdowns | undefined => {
    const current = load();
    return current.status === "ready" ? current.data : undefined;
  };
  const failure = (): string | undefined => {
    const current = load();
    return current.status === "error" ? current.message : undefined;
  };
  const days = createMemo(() => fillDays(data()?.day.rows ?? []));
  const today = (): string | undefined => {
    const current = load();
    return todayKey(days(), current.status === "ready" && current.endsNow);
  };
  const empty = (): boolean => {
    const totals = data()?.day.totals;
    return (
      totals !== undefined &&
      totals.cost_usd === 0 &&
      totals.tokens_in === 0 &&
      totals.tokens_out === 0
    );
  };

  return (
    <section class="min-h-0 flex-1 overflow-y-auto overscroll-contain">
      <div class="mx-auto flex w-full max-w-4xl flex-col gap-6 px-3 py-3 lg:px-4 lg:py-8">
        <header class="flex flex-wrap items-center gap-2">
          <a href="/" aria-label="Back to sessions" class="btn btn-ghost px-0 lg:hidden">
            <span aria-hidden="true">←</span>
          </a>
          <h1 class="flex-1 text-xl font-semibold">Usage</h1>
          <div role="group" aria-label="Range" class="flex gap-1">
            <For each={USAGE_RANGES}>
              {(days) => (
                <button
                  type="button"
                  class="btn px-3 text-sm"
                  aria-pressed={range() === days ? "true" : "false"}
                  onClick={() => setSearch({ days: String(days) })}
                >
                  {days} days
                </button>
              )}
            </For>
          </div>
        </header>

        <Switch>
          <Match when={failure()}>
            {(message) => (
              <div role="alert" class="flex flex-col items-start gap-2 text-sm">
                <p class="wrap-break-word">The usage could not be loaded: {message()}</p>
                <button type="button" class="btn" onClick={() => setAttempt((n) => n + 1)}>
                  Try again
                </button>
              </div>
            )}
          </Match>
          <Match when={load().status === "loading"}>
            <p class="text-sm text-muted">Loading usage…</p>
          </Match>
          <Match when={data()}>
            {(loaded) => (
              <>
                <section aria-label="Totals" class="flex flex-col gap-2">
                  <dl class="grid grid-cols-2 gap-3 sm:grid-cols-4" data-part="totals">
                    <div class="rounded-md border border-line bg-surface p-3">
                      <dt class="text-xs text-muted">Cost</dt>
                      <dd class="font-mono text-xl font-semibold">
                        {formatCost(loaded().day.totals.cost_usd)}
                      </dd>
                    </div>
                    <div class="rounded-md border border-line bg-surface p-3">
                      <dt class="text-xs text-muted">Input tokens</dt>
                      <dd class="font-mono text-xl font-semibold">
                        <Tokens count={loaded().day.totals.tokens_in} />
                      </dd>
                    </div>
                    <div class="rounded-md border border-line bg-surface p-3">
                      <dt class="text-xs text-muted">Output tokens</dt>
                      <dd class="font-mono text-xl font-semibold">
                        <Tokens count={loaded().day.totals.tokens_out} />
                      </dd>
                    </div>
                    <div class="rounded-md border border-line bg-surface p-3">
                      <dt class="text-xs text-muted">Sessions</dt>
                      <dd class="font-mono text-xl font-semibold">
                        {loaded().day.totals.sessions}
                      </dd>
                    </div>
                  </dl>
                  <p class="text-xs text-muted">
                    As reported by the agent CLI. On a subscription plan this estimates the value
                    used; it is not a bill.
                  </p>
                </section>

                <Show
                  when={!empty()}
                  fallback={<p class="text-sm">Nothing was used in this range.</p>}
                >
                  <section class="flex flex-col gap-2">
                    <h2 class="font-semibold">By day</h2>
                    <DayChart days={days()} today={today()} />
                  </section>

                  <section class="flex flex-col gap-2">
                    <h2 class="font-semibold">By session</h2>
                    <Show when={loaded().session.rows.length >= SESSION_CAP}>
                      <p class="text-xs text-muted">
                        The {SESSION_CAP} most expensive sessions, the most the server lists.
                      </p>
                    </Show>
                    <div>
                      <table class="w-full text-sm" data-part="by-session">
                        <caption class="sr-only">Most expensive sessions</caption>
                        <thead class="text-left text-xs text-muted">
                          <tr>
                            <th scope="col" class="py-1 font-medium">
                              Session
                            </th>
                            <th scope="col" class="py-1 text-right font-medium">
                              Cost
                            </th>
                            <th
                              scope="col"
                              class="py-1 pl-2 text-right font-medium whitespace-nowrap"
                            >
                              Tokens in / out
                            </th>
                          </tr>
                        </thead>
                        <tbody class="divide-y divide-line">
                          <For each={loaded().session.rows} keyed={(row) => row.key}>
                            {(row) => (
                              <tr>
                                <th scope="row" class="w-full max-w-0 pr-2 text-left font-normal">
                                  <a
                                    href={sessionPath(row().key)}
                                    class="flex min-h-11 items-center text-accent underline"
                                  >
                                    <span class="truncate">{row().label || row().key}</span>
                                  </a>
                                </th>
                                <td class="text-right font-mono whitespace-nowrap">
                                  {formatCost(row().totals.cost_usd)}
                                </td>
                                <td class="pl-2 text-right font-mono whitespace-nowrap">
                                  <Tokens count={row().totals.tokens_in} /> /{" "}
                                  <Tokens count={row().totals.tokens_out} />
                                </td>
                              </tr>
                            )}
                          </For>
                        </tbody>
                      </table>
                    </div>
                  </section>

                  <div class="grid gap-6 lg:grid-cols-2">
                    <ShareTable
                      caption="By repo"
                      heading="Repo"
                      rows={loaded().repo.rows}
                      emptyLabel="No repo"
                    />
                    <ShareTable
                      caption="By model"
                      heading="Model"
                      rows={loaded().model.rows}
                      emptyLabel="Unknown model"
                    />
                  </div>
                </Show>
              </>
            )}
          </Match>
        </Switch>

        <section class="flex flex-col gap-2">
          <h2 class="font-semibold">Plan limits</h2>
          <LimitMeters limits={app.limits()} now={app.now()} />
        </section>
      </div>
    </section>
  );
}
