export default function NotFoundPage() {
  return (
    <div class="flex flex-1 flex-col items-center justify-center gap-3 p-6 text-center">
      <h1 class="text-xl font-semibold">Page not found</h1>
      <p class="max-w-sm text-sm text-muted">Nothing lives at this address.</p>
      <a href="/" class="btn">
        Back to sessions
      </a>
    </div>
  );
}
