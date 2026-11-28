import { createSignal, type Accessor, type Setter } from "solid-js";
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
  /** Drops what was read, as when the sign-in ended; nothing is read again until `load`. */
  forget(): void;
};

/** `list` with `repo` in place of the one of the same id, or added. Pure. */
export const withRepo = (list: readonly Repo[], repo: Repo): Repo[] => [
  ...list.filter((one) => one.id !== repo.id),
  repo,
];

/** `list` with repo `id` shown cloning, as a 202 says (api.rs), before the next read confirms it. Pure. */
export const markCloning = (list: readonly Repo[], id: string): Repo[] =>
  list.map((repo) => (repo.id === id ? { ...repo, cloning: true, problem: null } : repo));

export function createCatalog(api: Api): Catalog {
  return new RepoCatalog(api);
}

// A class rather than a closure of methods: each method then stands, and is measured, on its own.
// `private`, not `#`: the complexity report does not see `#` methods.
class RepoCatalog implements Catalog {
  readonly repos: Accessor<RepoRegistry>;
  readonly sets: Accessor<readonly RepoSet[]>;
  private readonly setRepos: Setter<RepoRegistry>;
  private readonly setSets: Setter<readonly RepoSet[]>;
  private registry: readonly Repo[] | undefined;
  private wanted = false;
  private inFlight = false;
  // Set when the list changed under a read in flight: its answer is from before, so it is asked again.
  private outdated = false;
  private active = true;
  private poll: ReturnType<typeof setTimeout> | undefined;

  private readonly api: Api;

  constructor(api: Api) {
    this.api = api;
    [this.repos, this.setRepos] = createSignal<RepoRegistry>({ status: "loading" });
    [this.sets, this.setSets] = createSignal<readonly RepoSet[]>([]);
  }

  load(): void {
    this.wanted = true;
    if (!this.registry) this.setRepos({ status: "loading" });
    this.readAll();
  }

  reload(options?: { missingOnly?: boolean }): void {
    if (!this.wanted || (options?.missingOnly === true && this.registry)) return;
    this.readAll();
  }

  invalidate(): void {
    if (!this.wanted) return;
    if (this.inFlight) this.outdated = true;
    this.readAll();
  }

  async register(request: RegisterRepoRequest): Promise<Repo> {
    const repo = await this.api.registerRepo(request);
    // Without a list to add to, the server's own is the only complete one.
    this.change((list) => withRepo(list, repo));
    return repo;
  }

  async remove(id: string): Promise<void> {
    await this.api.removeRepo(id);
    if (this.registry) this.publish(this.registry.filter((one) => one.id !== id));
  }

  async clone(id: string): Promise<void> {
    await this.api.cloneRepo(id);
    this.change((list) => markCloning(list, id));
  }

  async rescan(): Promise<void> {
    const loaded = await this.api.refreshRepos();
    this.wanted = true;
    this.publish(loaded);
    this.readSets();
  }

  setActive(active: boolean): void {
    this.active = active;
    this.schedulePoll();
  }

  forget(): void {
    this.wanted = false;
    this.registry = undefined;
    this.active = false;
    this.schedulePoll();
    this.setRepos({ status: "loading" });
    this.setSets([]);
  }

  /** Applies `edit` to the list on screen, or reads the server's when there is none yet. */
  private change(edit: (list: readonly Repo[]) => readonly Repo[]): void {
    if (this.registry) this.publish(edit(this.registry));
    else this.readRepos();
  }

  private readAll(): void {
    this.readRepos();
    this.readSets();
  }

  private cloning(): boolean {
    return this.active && this.registry?.some((repo) => repo.cloning) === true;
  }

  private schedulePoll(): void {
    if (!this.cloning()) {
      clearTimeout(this.poll);
      this.poll = undefined;
      return;
    }
    this.poll ??= setTimeout(() => {
      this.poll = undefined;
      this.readRepos();
    }, CLONE_POLL_MS);
  }

  private publish(next: readonly Repo[]): void {
    this.registry = next.toSorted(compareRepos);
    if (this.inFlight) this.outdated = true;
    this.setRepos({ status: "ready", repos: this.registry });
    this.schedulePoll();
  }

  private readRepos(): void {
    if (this.inFlight) return;
    this.inFlight = true;
    this.outdated = false;
    this.api.listRepos().then(
      (loaded) => this.answered(loaded),
      (error: unknown) => this.failed(error),
    );
  }

  private answered(loaded: readonly Repo[]): void {
    this.inFlight = false;
    if (this.outdated) this.readRepos();
    else this.publish(loaded);
  }

  private failed(error: unknown): void {
    this.inFlight = false;
    // A list that was read once stays usable through an outage.
    if (!this.registry) this.setRepos({ status: "error", message: errorMessage(error) });
    this.schedulePoll();
  }

  private readSets(): void {
    // Sets are shortcuts over the list: without them the picker works the same, so a failure is silent.
    this.api.listRepoSets().then(
      (sets) => this.setSets(sets),
      () => {},
    );
  }
}
