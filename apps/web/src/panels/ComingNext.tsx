/** Stands in for a panel whose screen is not built yet. */
export function ComingNext(props: { title: string; detail: string }) {
  return (
    <div class="flex flex-1 flex-col items-center justify-center gap-2 p-6 text-center">
      <h2 class="font-semibold">{props.title}</h2>
      <p class="max-w-sm text-sm text-muted">{props.detail}</p>
      <p class="text-xs text-muted">Coming next.</p>
    </div>
  );
}
