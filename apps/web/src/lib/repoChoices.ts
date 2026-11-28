import type { Repo } from "../gen/Repo";
import type { RepoMode } from "../gen/RepoMode";
import type { RepoSet } from "../gen/RepoSet";
import type { RepoChoice } from "./prefs";
import { defaultRepoMode } from "./status";

// What the repo picker does to a selection. `choices` is in attach order: the
// first one is the primary repo, the agent's working directory.

const without = (choices: readonly RepoChoice[], id: string): RepoChoice[] =>
  choices.filter((choice) => choice.repoId !== id);

const isChosen = (choices: readonly RepoChoice[], id: string): boolean =>
  choices.some((choice) => choice.repoId === id);

/** The home repo is where the agent starts, so it goes first; the others in the order chosen. */
export function toggled(choices: readonly RepoChoice[], repo: Repo, chosen: boolean): RepoChoice[] {
  const rest = without(choices, repo.id);
  if (!chosen) return rest;
  const choice = { repoId: repo.id, mode: defaultRepoMode(repo) };
  return repo.home ? [choice, ...rest] : [...rest, choice];
}

export const withMode = (
  choices: readonly RepoChoice[],
  id: string,
  mode: RepoMode,
): RepoChoice[] => choices.map((choice) => (choice.repoId === id ? { ...choice, mode } : choice));

/** `undefined` when `id` is not chosen: there is nothing to move. */
export function madePrimary(choices: readonly RepoChoice[], id: string): RepoChoice[] | undefined {
  const choice = choices.find((one) => one.repoId === id);
  return choice ? [choice, ...without(choices, id)] : undefined;
}

/** A set's repos as the catalog knows them: those that can be attached, and those not cloned. */
export function setMembers(
  set: RepoSet,
  byId: ReadonlyMap<string, Repo>,
): { available: Repo[]; missing: Repo[] } {
  const known = set.repo_ids.flatMap((id) => byId.get(id) ?? []);
  return {
    available: known.filter((repo) => repo.available),
    missing: known.filter((repo) => !repo.available),
  };
}

/** A set counts as chosen while every repo of it that can be attached is. */
export function setChosen(
  choices: readonly RepoChoice[],
  set: RepoSet,
  byId: ReadonlyMap<string, Repo>,
): boolean {
  const { available } = setMembers(set, byId);
  return available.length > 0 && available.every((repo) => isChosen(choices, repo.id));
}

/** Choosing a chosen set removes its repos; any other set adds the ones missing, each as a worktree. */
export function toggledSet(
  choices: readonly RepoChoice[],
  set: RepoSet,
  byId: ReadonlyMap<string, Repo>,
): RepoChoice[] {
  if (setChosen(choices, set, byId)) {
    return choices.filter((choice) => !set.repo_ids.includes(choice.repoId));
  }
  const added = setMembers(set, byId).available.filter((repo) => !isChosen(choices, repo.id));
  return [...choices, ...added.map((repo): RepoChoice => ({ repoId: repo.id, mode: "write" }))];
}

const listed = (names: readonly string[]): string =>
  names.length <= 1 ? names.join("") : `${names.slice(0, -1).join(", ")} and ${names.at(-1)}`;

/** Which repos of a set were left out because they are not cloned; nothing once they are. */
export function leftOutNote(set: RepoSet, byId: ReadonlyMap<string, Repo>): string | undefined {
  const names = setMembers(set, byId).missing.map((repo) => repo.name);
  if (names.length === 0) return undefined;
  const [is, was] = names.length === 1 ? ["is", "was"] : ["are", "were"];
  return `${set.name}: ${listed(names)} ${is} not cloned and ${was} left out.`;
}

/** What is selected, in a line: the primary repo by name, the others counted. */
export function selectionSummary(names: readonly string[]): string {
  const [first] = names;
  if (first === undefined) return "none selected";
  const others = names.length - 1;
  return others === 0 ? first : `${first} + ${others} ${others === 1 ? "repo" : "repos"}`;
}

/** The repos whose name holds the filter, whatever the case; a short list is never filtered. */
export function filteredRepos(
  repos: readonly Repo[],
  filter: string,
  filterFrom: number,
): readonly Repo[] {
  const wanted = filter.trim().toLowerCase();
  if (wanted === "" || repos.length < filterFrom) return repos;
  return repos.filter((repo) => repo.name.toLowerCase().includes(wanted));
}
