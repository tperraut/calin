import { For, Show } from "solid-js";
import type { RunnerLimits } from "../gen/RunnerLimits";
import { absoluteTime } from "../lib/format";
import {
  meterState,
  percentUsed,
  refusalText,
  reportedText,
  resetText,
  windowName,
  type MeterState,
} from "../lib/usage";

const toneText: Record<MeterState["tone"], string> = {
  success: "text-tone-success",
  attention: "text-tone-attention",
  danger: "text-tone-danger",
};
const toneFill: Record<MeterState["tone"], string> = {
  success: "bg-tone-success",
  attention: "bg-tone-attention",
  danger: "bg-tone-danger",
};

/**
 * The plan's limit windows, one meter each, per runner. Text first: the
 * window, how much is used, when it resets and the state in words; the bar
 * only echoes them.
 */
export function LimitMeters(props: { limits: readonly RunnerLimits[]; now: number }) {
  return (
    <Show
      when={props.limits.some((report) => report.windows.length > 0)}
      fallback={
        <p class="text-sm text-muted">
          No limits reported yet. They appear after an agent has run.
        </p>
      }
    >
      <div class="flex flex-col gap-4">
        <For each={props.limits} keyed={(report) => report.runner}>
          {(report) => (
            <Show when={report().windows.length > 0}>
              <section aria-label={`Limits of ${report().runner}`} class="flex flex-col gap-3">
                <Show when={props.limits.length > 1}>
                  <h3 class="font-mono text-sm">{report().runner}</h3>
                </Show>
                <ul class="flex flex-col gap-3">
                  <For each={report().windows} keyed={(window) => window.name}>
                    {(window) => {
                      const state = () => meterState(window());
                      return (
                        <li data-part="meter" class="flex flex-col gap-1">
                          <p class="flex flex-wrap items-baseline justify-between gap-x-3">
                            <span>
                              <span class="font-medium">{windowName(window().name)}</span>:{" "}
                              {percentUsed(window().used)} used
                              <Show when={resetText(window(), props.now)}>
                                {(reset) => <>, {reset()}</>}
                              </Show>
                            </span>
                            <span class={["text-xs font-medium", toneText[state().tone]]}>
                              {state().label}
                            </span>
                          </p>
                          <div
                            aria-hidden="true"
                            class="h-2 overflow-hidden rounded-full bg-sunken"
                          >
                            <div
                              class={["h-full rounded-full", toneFill[state().tone]]}
                              style={{ width: percentUsed(window().used) }}
                            />
                          </div>
                          <Show when={window().refused}>
                            <p role="status" class="text-sm font-medium text-tone-danger">
                              {refusalText(window(), props.now)}
                            </p>
                          </Show>
                          <p class="text-xs text-muted">
                            <time
                              datetime={new Date(report().reported_at).toISOString()}
                              title={absoluteTime(report().reported_at)}
                            >
                              {reportedText(report().reported_at, props.now)}
                            </time>
                          </p>
                        </li>
                      );
                    }}
                  </For>
                </ul>
              </section>
            </Show>
          )}
        </For>
      </div>
    </Show>
  );
}
