import { createSignal, type Accessor } from "solid-js";
import type { Api } from "../api/http";
import type { RegisterRepoRequest } from "../gen/RegisterRepoRequest";
import type { Repo } from "../gen/Repo";
import type { RepoSet } from "../gen/RepoSet";
import { errorMessage } from "../lib/errors";

/** The repos the server knows, as `GET /repos` last answered. */
export type RepoRegistry =
  | { status: "loading" }
  | { status: "ready"; repos: readonly Repo[] }
  | { status: "error"; message: string };

/** How often the list is read again while a clone is running. */
export const CLONE_POLL_MS = 2000;

/** api.rs: the home repo first, then by name. */
export function compareRepos(a: Repo, b: Repo): number {
  if (a.home !== b.home) return a.home ? -1 : 1;
  return a.name < b.name ? -1 : a.name > b.name ? 1 : 0;
}

/**
 * The repo catalog: what was registered by hand and what the home repo
 * declares, with the home repo's sets. Read on demand, then kept fresh.
 */
export type Catalog = {
  repos: Accessor<RepoRegistry>;
  /** Empty until read, and when there is no home repo. */
  sets: Accessor<readonly RepoSet[]>;
  /** A screen wants the catalog: reads it, and it is kept fresh from then on. */
  load(): void;
  /** Reads again what a screen asked for; with `missingOnly`, only a list that never arrived. */
  reload(options?: { missingOnly?: boolean }): void;
  /** The server's list changed (settings were saved): reads it again, dropping an answer already on its way. */
  invalidate(): void;
  register(request: RegisterRepoRequest): Promise<Repo>;
  /** Forgets the registry entry; the clone on disk is untouched. */
  remove(id: string): Promise<void>;
  /** Starts a clone. The list is then read every `CLONE_POLL_MS` until no repo is cloning. */
  clone(id: string): Promise<void>;
  /** Has the server re-read the home repo and rescan, and takes its answer as the list. */
  rescan(): Promise<void>;
  /** While inactive nothing is polled. */
  setActive(active: boolean): void;
};

export function createCatalog(api: Api): Catalog {
  const [repos, setRepos] = createSignal<RepoRegistry>({ status: "loading" });
  const [sets, setSets] = createSignal<readonly RepoSet[]>([]);

  let registry: readonly Repo[] | undefined;
  let wanted = false;
  let inFlight = false;
  // Set when the list changed under a read in flight: its answer is from before, so it is asked again.
  let outdated = false;
  let active = true;
  let poll: ReturnType<typeof setTimeout> | undefined;

  function schedulePoll(): void {
    const cloning = active && registry?.some((repo) => repo.cloning) === true;
    if (!cloning) {
      if (poll !== undefined) clearTimeout(poll);
      poll = undefined;
      return;
    }
    if (poll !== undefined) return;
    poll = setTimeout(() => {
      poll = undefined;
      readRepos();
    }, CLONE_POLL_MS);
  }

  function publish(next: readonly Repo[]): void {
    registry = next.toSorted(compareRepos);
    if (inFlight) outdated = true;
    setRepos({ status: "ready", repos: registry });
    schedulePoll();
  }

  function readRepos(): void {
    if (inFlight) return;
    inFlight = true;
    outdated = false;
    api.listRepos().then(
      (loaded) => {
        inFlight = false;
        if (outdated) readRepos();
        else publish(loaded);
      },
      (error: unknown) => {
        inFlight = false;
        // A list that was read once stays usable through an outage.
        if (!registry) setRepos({ status: "error", message: errorMessage(error) });
        schedulePoll();
      },
    );
  }

  function readSets(): void {
    // Sets are shortcuts over the list: without them the picker works the same, so a failure is silent.
    api.listRepoSets().then(setSets, () => {});
  }

  return {
    repos,
    sets,
    load() {
      wanted = true;
      if (!registry) setRepos({ status: "loading" });
      readRepos();
      readSets();
    },
    reload(options) {
      if (!wanted || (options?.missingOnly === true && registry)) return;
      readRepos();
      readSets();
    },
    invalidate() {
      if (!wanted) return;
      if (inFlight) outdated = true;
      readRepos();
      readSets();
    },
    async register(request) {
      const repo = await api.registerRepo(request);
      // Without a list to add to, the server's own is the only complete one.
      if (registry) publish([...registry.filter((one) => one.id !== repo.id), repo]);
      else readRepos();
      return repo;
    },
    async remove(id) {
      await api.removeRepo(id);
      if (registry) publish(registry.filter((one) => one.id !== id));
    },
    async clone(id) {
      await api.cloneRepo(id);
      // 202 means the clone is running (api.rs): say so now, the next read confirms it.
      if (registry) {
        publish(
          registry.map((repo) =>
            repo.id === id ? { ...repo, cloning: true, problem: null } : repo,
          ),
        );
      } else readRepos();
    },
    async rescan() {
      const loaded = await api.refreshRepos();
      wanted = true;
      publish(loaded);
      readSets();
    },
    setActive(next) {
      active = next;
      schedulePoll();
    },
  };
}
